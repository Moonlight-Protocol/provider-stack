/**
 * Deterministic UTXO keys for the entity payment surface (#/pay-utxo).
 *
 * The seed is client-only: the wallet's signature never leaves this module,
 * and only the client can regenerate it — it exists to generate UTXO keys
 * and to find the balance by sweeping the derived keys in index order.
 * Derivation mechanism matches moonlight-pay's self-custodial flow:
 *   - master seed = SHA-256(signMessage(SEED_MESSAGE))
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

export const SEED_MESSAGE = "Moonlight: Derive UTXO seed";
const BATCH_SIZE = 10;

export interface ChannelIds {
  channelContractId: string;
  channelAuthId: string;
  assetContractId: string;
  /** Asset code from the council's channel config (e.g. "XLM"). */
  assetCode?: string;
  label?: string;
}

export interface DerivedUtxo {
  index: number;
  keypair: UTXOKeypairBase;
  /**
   * On-chain balance from the last refresh. -1 = never existed (the
   * contract's marker, also the initial state before a read) — the ONLY
   * state a key may be re-used from. 0 = exists but spent: re-CREATEing it
   * fails on-chain with UTXOAlreadyExists(#1).
   */
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

/**
 * Wrap a raw P-256 scalar in a PKCS8 envelope — the SDK's signPayload
 * imports the key as pkcs8 (same wrapper as moonlight-pay's
 * selfcustodial-payment.ts buildPkcs8P256).
 */
function pkcs8P256(rawPrivateKey: Uint8Array): Uint8Array {
  // deno-fmt-ignore
  const header = new Uint8Array([
    0x30, 0x41, 0x02, 0x01, 0x00, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48,
    0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03,
    0x01, 0x07, 0x04, 0x27, 0x30, 0x25, 0x02, 0x01, 0x01, 0x04, 0x20,
  ]);
  const out = new Uint8Array(header.length + 32);
  out.set(header);
  out.set(rawPrivateKey, header.length);
  return out;
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
  const raw = new Uint8Array(expanded).slice(0, 32);
  const { p256 } = await import("@noble/curves/p256");
  const publicKey = new Uint8Array(
    p256.ProjectivePoint.fromPrivateKey(raw).toRawBytes(false),
  );
  // signPayload imports the key as PKCS8, not as a raw scalar.
  return { privateKey: pkcs8P256(raw), publicKey };
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
    balance: -1n,
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

/**
 * Re-read utxo_balances across derived keys, extending the derived set with
 * a gap limit: keep scanning further batches until MIN_SCAN_BATCHES have
 * been read AND the latest batch holds no funded key. The contract returns
 * -1 for keys that never existed — that counts as unfunded/free, not as a
 * balance.
 */
const MIN_SCAN_BATCHES = 3;

export async function refreshBalances(ids: ChannelIds): Promise<void> {
  const client = channelClient(ids);
  let batch = 0;
  while (true) {
    while (derived.length < (batch + 1) * BATCH_SIZE) await deriveBatch();
    const slice = derived.slice(batch * BATCH_SIZE, (batch + 1) * BATCH_SIZE);
    const balances = await client.read({
      // deno-lint-ignore no-explicit-any
      method: "utxo_balances" as any,
      methodArgs: {
        utxos: slice.map((d) => Buffer.from(d.keypair.publicKey)),
      },
      // deno-lint-ignore no-explicit-any
    } as any) as bigint[];
    balances.forEach((b, i) => {
      slice[i].balance = b ?? -1n;
      // A reservation protects an in-flight CREATE; once the key exists
      // on-chain that job is done — clear it so the funds are spendable
      // this session. Keys still at -1 (a handed-out payment code the
      // payer hasn't executed) stay reserved.
      if (slice[i].balance >= 0n) slice[i].reserved = false;
    });
    // Keep scanning while the batch holds any EXISTING key (funded or
    // spent) — stopping at "no funded" would leave existing keys past the
    // boundary unread, and the -1 frontier must be truly never-used.
    const batchTouched = slice.some((d) => d.balance >= 0n);
    batch++;
    if (batch >= MIN_SCAN_BATCHES && !batchTouched) return;
  }
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

/**
 * Reserve `n` never-used keys. Only balance -1 (the contract's "does not
 * exist" marker) qualifies: a balance of exactly 0 means the UTXO exists
 * but was spent, and re-CREATEing a spent UTXO fails the whole bundle
 * on-chain with UTXOAlreadyExists(#1).
 */
export async function reserveFreeUtxos(n: number): Promise<DerivedUtxo[]> {
  const out: DerivedUtxo[] = [];
  while (out.length < n) {
    const free = derived.find((d) => d.balance < 0n && !d.reserved);
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
