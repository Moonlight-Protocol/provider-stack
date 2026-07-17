/**
 * Deterministic UTXO keys for the entity payment surface (#/pay-utxo).
 *
 * Derivation matches moonlight-pay's self-custodial flow exactly:
 *   - master seed = SHA-256(signMessage("Moonlight: Derive server key"))
 *     (moonlight-pay wallet.ts initMasterSeed / wallet-state.ts)
 *   - per-index key = HKDF-SHA256(SHA-256(seed || String(index)),
 *     info "moonlight-p256") → first 32 bytes → P-256 keypair
 *     (moonlight-pay selfcustodial-payment.ts deriveUtxoKeypairs)
 *
 * Same wallet + same channel = same UTXO set every visit, so funds are not
 * limited to a session: balances come from the channel's on-chain
 * utxo_balances over the derived keys. The seed itself stays module-local
 * (memory only, re-derived each visit via one wallet signature) — the
 * zero-persistence stance is unchanged.
 */
import { PrivacyChannel, UTXOKeypairBase } from "@moonlight/moonlight-sdk";
import { NetworkConfig } from "@colibri/core";
import { Buffer } from "buffer";
import { HORIZON_URL, RPC_URL } from "./config.ts";
import { getNetworkPassphrase } from "./wallet.ts";
import { signEntityMessage } from "./wallet-entity.ts";

export const SEED_MESSAGE = "Moonlight: Derive server key";
const BATCH_SIZE = 10;

export interface ChannelIds {
  channelContractId: string;
  channelAuthId: string;
  assetContractId: string;
}

export interface DerivedUtxo {
  index: number;
  keypair: UTXOKeypairBase;
  /** On-chain balance from the last refresh; 0 = unused (or spent). */
  balance: bigint;
  /** Reserved for an in-flight op (deposit/change/receive) this visit. */
  reserved: boolean;
}

let masterSeed: Uint8Array | null = null;
let derived: DerivedUtxo[] = [];

export function clearDerivation(): void {
  if (masterSeed) masterSeed.fill(0);
  masterSeed = null;
  derived = [];
}

export function isSeedReady(): boolean {
  return masterSeed !== null;
}

// ── seed + key derivation (moonlight-pay parity) ───────────────

function base64UrlToBytes(s: string): Uint8Array {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/");
  const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
  return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
}

async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  const buf = new ArrayBuffer(bytes.length);
  new Uint8Array(buf).set(bytes);
  return new Uint8Array(await crypto.subtle.digest("SHA-256", buf));
}

/** One wallet popup: sign the fixed derivation message, hash → master seed. */
export async function initEntitySeed(): Promise<void> {
  const signature = await signEntityMessage(SEED_MESSAGE);
  masterSeed = await sha256(base64UrlToBytes(signature));
  derived = [];
}

async function deriveP256(
  seed: Uint8Array,
): Promise<{ privateKey: Uint8Array; publicKey: Uint8Array }> {
  const seedBuf = new ArrayBuffer(seed.length);
  new Uint8Array(seedBuf).set(seed);
  const expandKey = await crypto.subtle.importKey(
    "raw",
    seedBuf,
    "HKDF",
    false,
    ["deriveBits"],
  );
  const expanded = await crypto.subtle.deriveBits(
    {
      name: "HKDF",
      hash: "SHA-256",
      salt: new Uint8Array(0),
      info: new TextEncoder().encode("moonlight-p256"),
    },
    expandKey,
    384,
  );
  const privateKey = new Uint8Array(expanded).slice(0, 32);
  const { p256 } = await import("@noble/curves/p256");
  const publicKey = new Uint8Array(
    p256.ProjectivePoint.fromPrivateKey(privateKey).toRawBytes(false),
  );
  return { privateKey, publicKey };
}

async function deriveIndex(index: number): Promise<DerivedUtxo> {
  if (!masterSeed) throw new Error("Master seed not initialized");
  const indexBytes = new TextEncoder().encode(String(index));
  const seedInput = new Uint8Array(masterSeed.length + indexBytes.length);
  seedInput.set(masterSeed);
  seedInput.set(indexBytes, masterSeed.length);
  const kp = await deriveP256(await sha256(seedInput));
  return {
    index,
    keypair: new UTXOKeypairBase(kp),
    balance: 0n,
    reserved: false,
  };
}

/** Extend the derived set by one batch. */
export async function deriveBatch(): Promise<void> {
  const start = derived.length;
  for (let i = start; i < start + BATCH_SIZE; i++) {
    derived.push(await deriveIndex(i));
  }
}

// ── on-chain balances ──────────────────────────────────────────

function channelClient(ids: ChannelIds): PrivacyChannel {
  const networkConfig = NetworkConfig.CustomNet({
    networkPassphrase: getNetworkPassphrase(),
    rpcUrl: RPC_URL,
    horizonUrl: HORIZON_URL,
    friendbotUrl: "",
    allowHttp: true,
  });
  return new PrivacyChannel(
    networkConfig,
    ids.channelContractId as `C${string}`,
    ids.channelAuthId as `C${string}`,
    ids.assetContractId as `C${string}`,
  );
}

/** Re-read utxo_balances for every derived key. */
export async function refreshBalances(ids: ChannelIds): Promise<void> {
  if (derived.length === 0) await deriveBatch();
  const client = channelClient(ids);
  const balances = await client.read({
    // deno-lint-ignore no-explicit-any
    method: "utxo_balances" as any,
    methodArgs: {
      utxos: derived.map((d) => Buffer.from(d.keypair.publicKey)),
    },
    // deno-lint-ignore no-explicit-any
  } as any) as bigint[];
  balances.forEach((b, i) => {
    derived[i].balance = b ?? 0n;
  });
}

// ── selection ──────────────────────────────────────────────────

export function fundedUtxos(): DerivedUtxo[] {
  return derived.filter((d) => d.balance > 0n && !d.reserved);
}

export function balance(): bigint {
  return derived.filter((d) => d.balance > 0n)
    .reduce((acc, d) => acc + d.balance, 0n);
}

export function fundedCount(): number {
  return derived.filter((d) => d.balance > 0n).length;
}

/** Reserve `n` unused keys (balance 0, not already handed out). */
export async function reserveFreeUtxos(n: number): Promise<DerivedUtxo[]> {
  const out: DerivedUtxo[] = [];
  while (out.length < n) {
    const free = derived.find((d) => d.balance === 0n && !d.reserved);
    if (!free) {
      await deriveBatch();
      continue;
    }
    free.reserved = true;
    out.push(free);
  }
  return out;
}

/** Greedy selection of funded UTXOs covering `total`. Null if insufficient. */
export function selectFunded(
  total: bigint,
): { selected: DerivedUtxo[]; change: bigint } | null {
  const selected: DerivedUtxo[] = [];
  let acc = 0n;
  for (const u of fundedUtxos()) {
    selected.push(u);
    acc += u.balance;
    if (acc >= total) return { selected, change: acc - total };
  }
  return null;
}
