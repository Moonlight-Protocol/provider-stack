/**
 * Privacy-channel operations for the entity payment surface (#/pay-utxo).
 *
 * Mirrors the reference clients (local-dev/lib/client/{deposit,send,receive}.ts)
 * with the wallet in place of a raw Keypair, and moonlight-pay's derived
 * UTXO keys (lib/utxo-derivation.ts) in place of an account handler:
 *   - Deposit: DEPOSIT (Ed25519-authorized via the wallet's signAuthEntry)
 *     + CREATE at the next free derived key. Bundle = [deposit, create].
 *   - Request: reserve free derived key(s) → CREATE op(s), shared as MLXDR
 *     strings out of band. Re-derivable — no secret custody problem.
 *   - Send: parse receiver CREATE MLXDRs, SPEND funded derived UTXOs
 *     (P256-signed in memory, no wallet popup) + change CREATE at a free
 *     derived key. Bundle = [creates, spends].
 *
 * Wire shape (crates/api/src/routes/bundles.rs):
 *   POST /provider/entity/bundles { operationsMLXDR, channelContractId }
 */
import { MoonlightOperation } from "@moonlight/moonlight-sdk";
import { authorizeEntry, type xdr } from "stellar-sdk";
import { Buffer } from "buffer";
import { RPC_URL } from "./config.ts";
import { entityFetch } from "./entity-auth.ts";
import {
  getEntityAddress,
  signEntityAuthEntry,
  signEntityTransaction,
} from "./wallet-entity.ts";
import { getNetworkPassphrase } from "./wallet.ts";
import {
  type ChannelIds,
  type DerivedUtxo,
  reserveFreeUtxos,
  selectFunded,
} from "./utxo-derivation.ts";

// LOW-entropy fees, matching the reference clients (local-dev/lib/client).
export const DEPOSIT_FEE = 500_000n; // 0.05 XLM in stroops
export const SEND_FEE = 1_000_000n; // 0.1 XLM in stroops

// ── Amounts ────────────────────────────────────────────────────

export function toStroops(input: string): bigint {
  const m = input.trim().match(/^(\d+)(?:\.(\d{1,7}))?$/);
  if (!m) throw new Error(`Invalid amount: "${input}"`);
  return BigInt(m[1]) * 10_000_000n + BigInt((m[2] ?? "").padEnd(7, "0") || 0);
}

export function fromStroops(v: bigint): string {
  const sign = v < 0n ? "-" : "";
  const abs = v < 0n ? -v : v;
  const whole = abs / 10_000_000n;
  const frac = (abs % 10_000_000n).toString().padStart(7, "0").replace(
    /0+$/,
    "",
  );
  return `${sign}${whole}${frac ? `.${frac}` : ""}`;
}

// ── Ledger ─────────────────────────────────────────────────────

async function getLatestLedger(): Promise<number> {
  const res = await fetch(RPC_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "getLatestLedger" }),
  });
  if (!res.ok) throw new Error(`RPC getLatestLedger failed: ${res.status}`);
  const body = await res.json();
  const seq = body?.result?.sequence;
  if (typeof seq !== "number") {
    throw new Error("RPC getLatestLedger returned no sequence");
  }
  return seq;
}

// ── Wallet as a Colibri Signer ─────────────────────────────────

/**
 * Duck-typed Colibri `Signer` backed by the connected wallet. Only
 * signSorobanAuthEntry is exercised by the deposit path; raw `sign` is not
 * something a browser wallet exposes and throws if ever reached.
 */
function walletSigner(address: string) {
  return {
    publicKey: () => address,
    sign: (): Uint8Array => {
      throw new Error("Raw payload signing is not available via the wallet");
    },
    signTransaction: (tx: string | { toXDR(): string }) =>
      signEntityTransaction(typeof tx === "string" ? tx : tx.toXDR()),
    // Freighter's signAuthEntry signs the HashIdPreimage (and returns just
    // the signature), so the entry is authorized via stellar-sdk's
    // authorizeEntry with the wallet as the signing callback. authorizeEntry
    // verifies the returned signature against the depositor's key — a
    // malformed wallet response fails loudly here.
    signSorobanAuthEntry: (
      entry: xdr.SorobanAuthorizationEntry,
      validUntil: number,
      networkPassphrase: string,
    ) =>
      authorizeEntry(
        entry,
        async (preimage: xdr.HashIdPreimage) =>
          Buffer.from(
            await signEntityAuthEntry(
              preimage.toXDR("base64"),
              networkPassphrase,
            ),
            "base64",
          ),
        validUntil,
        networkPassphrase,
      ),
    signsFor: (target: string) => target === address,
  };
}

// ── Bundle submission / polling ────────────────────────────────

export type BundleStatus =
  | "PENDING"
  | "PROCESSING"
  | "COMPLETED"
  | "FAILED"
  | "EXPIRED";

export interface BundleState {
  id: string;
  status: BundleStatus;
  failureDetail: unknown;
}

async function submitBundle(
  operationsMLXDR: string[],
  channelContractId: string,
): Promise<string> {
  const res = await entityFetch("/provider/entity/bundles", {
    method: "POST",
    body: JSON.stringify({ operationsMLXDR, channelContractId }),
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(
      body.message || `Bundle submission failed: ${res.status}`,
    );
  }
  const { data } = await res.json();
  return data.operationsBundleId as string;
}

export async function getBundle(bundleId: string): Promise<BundleState> {
  const res = await entityFetch(
    `/provider/entity/bundles/${encodeURIComponent(bundleId)}`,
  );
  if (!res.ok) throw new Error(`Bundle fetch failed: ${res.status}`);
  const { data } = await res.json();
  return {
    id: data.id,
    status: data.status as BundleStatus,
    failureDetail: data.failureDetail ?? null,
  };
}

// ── Deposit ────────────────────────────────────────────────────

/**
 * Deposit `amount` stroops into the channel. One wallet popup (the Soroban
 * auth entry over the SAC transfer). The deposited amount lands on the next
 * free derived key; the fee difference stays with the provider.
 */
export async function submitDeposit(
  amount: bigint,
  channel: ChannelIds,
): Promise<string> {
  const account = getEntityAddress();
  if (!account) throw new Error("Wallet not connected");

  const [target] = await reserveFreeUtxos(1);
  const createOp = MoonlightOperation.create(target.keypair.publicKey, amount);

  const expiration = (await getLatestLedger()) + 1000;

  const depositOp = await MoonlightOperation.deposit(
    account as `G${string}`,
    amount + DEPOSIT_FEE,
  )
    .addConditions([createOp.toCondition()])
    .signWithEd25519(
      // deno-lint-ignore no-explicit-any
      walletSigner(account) as any,
      expiration,
      channel.channelContractId as `C${string}`,
      channel.assetContractId as `C${string}`,
      getNetworkPassphrase(),
    );

  return await submitBundle(
    [depositOp.toMLXDR(), createOp.toMLXDR()],
    channel.channelContractId,
  );
}

// ── Request (receive) ──────────────────────────────────────────

/**
 * Reserve `count` free derived keys and build receiving CREATE ops
 * totalling `amount`, split evenly (remainder on the last). Returns the
 * MLXDR strings to share with the payer. Keys are re-derivable from the
 * wallet — nothing to back up.
 */
export async function prepareReceive(
  amount: bigint,
  count: number,
): Promise<string[]> {
  if (count < 1 || !Number.isInteger(count)) {
    throw new Error("Key count must be a positive integer");
  }
  if (amount < BigInt(count)) {
    throw new Error("Amount too small to split across the requested keys");
  }
  const targets = await reserveFreeUtxos(count);
  const per = amount / BigInt(count);
  return targets.map((t, i) => {
    const share = i === count - 1 ? amount - per * BigInt(count - 1) : per;
    return MoonlightOperation.create(t.keypair.publicKey, share).toMLXDR();
  });
}

// ── Send ───────────────────────────────────────────────────────

export interface ParsedReceiverOps {
  ops: Array<{ publicKey: Uint8Array; amount: bigint }>;
  total: bigint;
}

/** Parse pasted receiver MLXDR lines; every line must be a CREATE op. */
export function parseReceiverOps(pasted: string): ParsedReceiverOps {
  const lines = pasted.split(/\s+/).map((l) => l.trim()).filter(Boolean);
  if (lines.length === 0) throw new Error("Paste the receiver's keys first");
  const ops = lines.map((mlxdr, i) => {
    let op;
    try {
      op = MoonlightOperation.fromMLXDR(mlxdr);
    } catch {
      throw new Error(`Line ${i + 1} is not a valid operation`);
    }
    if (!op.isCreate()) {
      throw new Error(`Line ${i + 1} is not a CREATE operation`);
    }
    return { publicKey: op.getUtxo(), amount: op.getAmount() };
  });
  return { ops, total: ops.reduce((acc, o) => acc + o.amount, 0n) };
}

export interface SubmittedSend {
  bundleId: string;
  spent: DerivedUtxo[];
}

/**
 * Send to the receiver's CREATE ops by spending funded derived UTXOs. No
 * wallet popup — SPENDs are signed with the in-memory P256 keys. Bundle
 * order matches the reference client: CREATEs first, then SPENDs.
 */
export async function submitSend(
  receivers: ParsedReceiverOps,
  channel: ChannelIds,
): Promise<SubmittedSend> {
  const totalToSpend = receivers.total + SEND_FEE;
  const selection = selectFunded(totalToSpend);
  if (!selection) {
    throw new Error(
      `Insufficient balance: need ${fromStroops(totalToSpend)} XLM (incl. ${
        fromStroops(SEND_FEE)
      } fee)`,
    );
  }

  const createOps = receivers.ops.map((o) =>
    MoonlightOperation.create(o.publicKey, o.amount)
  );

  if (selection.change > 0n) {
    const [changeTarget] = await reserveFreeUtxos(1);
    createOps.push(
      MoonlightOperation.create(
        changeTarget.keypair.publicKey,
        selection.change,
      ),
    );
  }

  const expiration = (await getLatestLedger()) + 1000;

  const spendOps = [];
  for (const utxo of selection.selected) {
    let spendOp = MoonlightOperation.spend(utxo.keypair.publicKey);
    for (const createOp of createOps) {
      spendOp = spendOp.addCondition(createOp.toCondition());
    }
    utxo.reserved = true;
    spendOps.push(
      await spendOp.signWithUTXO(
        utxo.keypair,
        channel.channelContractId as `C${string}`,
        expiration,
      ),
    );
  }

  const bundleId = await submitBundle(
    [
      ...createOps.map((op) => op.toMLXDR()),
      ...spendOps.map((op) => op.toMLXDR()),
    ],
    channel.channelContractId,
  );
  return { bundleId, spent: selection.selected };
}
