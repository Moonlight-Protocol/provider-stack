/**
 * Session-local UTXO state for the entity payment surface (#/pay-utxo).
 *
 * Same zero-persistence stance as the rest of the entity session: keypairs
 * and balances live in module-local memory only and vanish on refresh or
 * route re-entry. Keys are generated on demand (WebCrypto P-256) — sign-in
 * involves no key derivation.
 */
import { UTXOKeypairBase } from "@moonlight/moonlight-sdk";

export type SessionUtxoStatus = "PENDING" | "FUNDED" | "SPENT";

export interface SessionUtxo {
  keypair: UTXOKeypairBase;
  amount: bigint;
  status: SessionUtxoStatus;
}

/** UTXOs this session controls and funds via its own deposits/change. */
let ownUtxos: SessionUtxo[] = [];

export function clearUtxoSession(): void {
  ownUtxos = [];
}

export function addOwnUtxo(
  keypair: UTXOKeypairBase,
  amount: bigint,
): SessionUtxo {
  const utxo: SessionUtxo = { keypair, amount, status: "PENDING" };
  ownUtxos.push(utxo);
  return utxo;
}

export function fundedUtxos(): SessionUtxo[] {
  return ownUtxos.filter((u) => u.status === "FUNDED");
}

export function pendingAmount(): bigint {
  return ownUtxos
    .filter((u) => u.status === "PENDING")
    .reduce((acc, u) => acc + u.amount, 0n);
}

export function balance(): bigint {
  return fundedUtxos().reduce((acc, u) => acc + u.amount, 0n);
}

/** Greedy selection of funded UTXOs covering `total`. Null if insufficient. */
export function selectUtxos(
  total: bigint,
): { selected: SessionUtxo[]; change: bigint } | null {
  const selected: SessionUtxo[] = [];
  let acc = 0n;
  for (const u of fundedUtxos()) {
    selected.push(u);
    acc += u.amount;
    if (acc >= total) return { selected, change: acc - total };
  }
  return null;
}

// ── Key generation ─────────────────────────────────────────────

function b64urlToBytes(s: string): Uint8Array {
  const b64 = s.replace(/-/g, "+").replace(/_/g, "/");
  const padded = b64 + "=".repeat((4 - (b64.length % 4)) % 4);
  return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
}

/**
 * Generate a fresh random P-256 keypair via WebCrypto and wrap it in the
 * SDK's UTXOKeypairBase (raw 32-byte private key, 65-byte uncompressed
 * public key — the formats MoonlightOperation expects).
 */
export async function generateUtxoKeypair(): Promise<UTXOKeypairBase> {
  const kp = await crypto.subtle.generateKey(
    { name: "ECDSA", namedCurve: "P-256" },
    true,
    ["sign"],
  );
  const jwk = await crypto.subtle.exportKey("jwk", kp.privateKey);
  if (!jwk.d) throw new Error("WebCrypto did not export the private scalar");
  const privateKey = b64urlToBytes(jwk.d);
  const publicKey = new Uint8Array(
    await crypto.subtle.exportKey("raw", kp.publicKey),
  );
  return new UTXOKeypairBase({ privateKey, publicKey });
}

export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}
