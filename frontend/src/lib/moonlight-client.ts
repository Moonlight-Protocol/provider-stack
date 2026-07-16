/**
 * Privacy-channel operations for the entity payment surface (#/pay-utxo).
 *
 * Mirrors the reference clients (local-dev/lib/client/{deposit,send,receive}.ts)
 * with the wallet in place of a raw Keypair:
 *   - Deposit: DEPOSIT (Ed25519-authorized via the wallet's signAuthEntry)
 *     + CREATE at a fresh session key. Bundle = [deposit, create].
 *   - Request: fresh session key(s) → CREATE op(s), shared as MLXDR strings
 *     out of band. No submission.
 *   - Send: parse receiver CREATE MLXDRs, SPEND session UTXOs (P256-signed
 *     in memory, no wallet popup) + change CREATE. Bundle = [creates, spends].
 *
 * Wire shape (crates/api/src/routes/bundles.rs):
 *   POST /provider/entity/bundles { operationsMLXDR, channelContractId }
 */
import { MoonlightOperation } from "@moonlight/moonlight-sdk";
import { xdr } from "stellar-sdk";
import { RPC_URL } from "./config.ts";
import { entityFetch } from "./entity-auth.ts";
import {
  getEntityAddress,
  signEntityAuthEntry,
  signEntityTransaction,
} from "./wallet-entity.ts";
import { getNetworkPassphrase } from "./wallet.ts";
import {
  addOwnUtxo,
  generateUtxoKeypair,
  selectUtxos,
  type SessionUtxo,
} from "./utxo-session.ts";

// LOW-entropy fees, matching the reference clients (local-dev/lib/client).
export const DEPOSIT_FEE = 500_000n; // 0.05 XLM in stroops
export const SEND_FEE = 1_000_000n; // 0.1 XLM in stroops

export interface ChannelConfig {
  channelContractId: string;
  assetContractId: string;
}

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
    signSorobanAuthEntry: async (
      entry: string | { toXDR(format: "base64"): string },
      _validUntil: number,
      networkPassphrase: string,
    ) => {
      const b64 = typeof entry === "string" ? entry : entry.toXDR("base64");
      const signed = await signEntityAuthEntry(b64, networkPassphrase);
      return xdr.SorobanAuthorizationEntry.fromXDR(signed, "base64");
    },
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

export interface SubmittedDeposit {
  bundleId: string;
  utxo: SessionUtxo;
}

/**
 * Deposit `amount` stroops into the channel. One wallet popup (the Soroban
 * auth entry over the SAC transfer). The deposited amount lands on a fresh
 * session UTXO; the fee difference stays with the provider.
 */
export async function submitDeposit(
  amount: bigint,
  channel: ChannelConfig,
): Promise<SubmittedDeposit> {
  const account = getEntityAddress();
  if (!account) throw new Error("Wallet not connected");

  const keypair = await generateUtxoKeypair();
  const createOp = MoonlightOperation.create(keypair.publicKey, amount);

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

  const bundleId = await submitBundle(
    [depositOp.toMLXDR(), createOp.toMLXDR()],
    channel.channelContractId,
  );
  const utxo = addOwnUtxo(keypair, amount);
  return { bundleId, utxo };
}

// ── Request (receive) ──────────────────────────────────────────

export interface ReceiveRequest {
  mlxdrs: string[];
  secretsHex: string[];
}

/**
 * Generate `count` receiving CREATE ops totalling `amount`, split evenly
 * (remainder on the last). Returns the MLXDR strings to share with the payer
 * and the raw P256 secrets the recipient must keep to ever spend the funds.
 */
export async function prepareReceive(
  amount: bigint,
  count: number,
): Promise<ReceiveRequest> {
  if (count < 1 || !Number.isInteger(count)) {
    throw new Error("Key count must be a positive integer");
  }
  if (amount < BigInt(count)) {
    throw new Error("Amount too small to split across the requested keys");
  }
  const per = amount / BigInt(count);
  const mlxdrs: string[] = [];
  const secretsHex: string[] = [];
  for (let i = 0; i < count; i++) {
    const keypair = await generateUtxoKeypair();
    const share = i === count - 1 ? amount - per * BigInt(count - 1) : per;
    mlxdrs.push(MoonlightOperation.create(keypair.publicKey, share).toMLXDR());
    secretsHex.push(
      Array.from(keypair.privateKey, (b) => b.toString(16).padStart(2, "0"))
        .join(""),
    );
  }
  return { mlxdrs, secretsHex };
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
  change: SessionUtxo | null;
  spent: SessionUtxo[];
}

/**
 * Send to the receiver's CREATE ops by spending session UTXOs. No wallet
 * popup — SPENDs are signed with the in-memory P256 keys. Bundle order
 * matches the reference client: CREATEs first, then SPENDs.
 */
export async function submitSend(
  receivers: ParsedReceiverOps,
  channel: ChannelConfig,
): Promise<SubmittedSend> {
  const totalToSpend = receivers.total + SEND_FEE;
  const selection = selectUtxos(totalToSpend);
  if (!selection) {
    throw new Error(
      `Insufficient session balance: need ${
        fromStroops(totalToSpend)
      } XLM (incl. ${fromStroops(SEND_FEE)} fee)`,
    );
  }

  const createOps = receivers.ops.map((o) =>
    MoonlightOperation.create(o.publicKey, o.amount)
  );

  let change: SessionUtxo | null = null;
  if (selection.change > 0n) {
    const changeKeypair = await generateUtxoKeypair();
    createOps.push(
      MoonlightOperation.create(changeKeypair.publicKey, selection.change),
    );
    change = addOwnUtxo(changeKeypair, selection.change);
  }

  const expiration = (await getLatestLedger()) + 1000;

  const spendOps = [];
  for (const utxo of selection.selected) {
    let spendOp = MoonlightOperation.spend(utxo.keypair.publicKey);
    for (const createOp of createOps) {
      spendOp = spendOp.addCondition(createOp.toCondition());
    }
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
  return { bundleId, change, spent: selection.selected };
}
