/**
 * Entity SEP-10 auth client for the provider-stack entity surface.
 *
 * Backend contract (crates/api/src/routes/auth_stellar.rs):
 *   GET  /stellar/auth?account=G...        → { data: { challenge, networkPassphrase } }
 *   POST /stellar/auth { signedChallenge } → { data: { jwt } }
 *
 * Isolation contract — same stance as wallet-kyc.ts: the entity JWT lives in
 * MODULE-LOCAL state only. No localStorage or any other persistence, and the
 * operator session (console_token in api.ts) is never read or written.
 * Refresh purges the session — the entity re-signs SEP-10 every visit.
 */
import { API_BASE_URL } from "./config.ts";
import { currentTraceparent } from "./tracer.ts";
import { getEntityAddress, signEntityTransaction } from "./wallet-entity.ts";
import { getNetworkPassphrase } from "./wallet.ts";

let entityToken: string | null = null;

function withTraceparent(
  headers: Record<string, string>,
): Record<string, string> {
  const tp = currentTraceparent();
  return tp ? { ...headers, traceparent: tp } : headers;
}

function decodeJwtPayload(token: string): Record<string, unknown> {
  const b64 = token.split(".")[1].replace(/-/g, "+").replace(/_/g, "/");
  return JSON.parse(atob(b64));
}

/**
 * Authenticate as an entity via SEP-10 challenge-response.
 * Fetches the server-signed challenge, co-signs it with the entity wallet,
 * and exchanges it for an entity JWT.
 */
export async function authenticateEntity(): Promise<string> {
  const account = getEntityAddress();
  if (!account) throw new Error("Wallet not connected");

  const challengeRes = await fetch(
    `${API_BASE_URL}/stellar/auth?account=${encodeURIComponent(account)}`,
    { headers: withTraceparent({}) },
  );
  if (!challengeRes.ok) {
    throw new Error(`Failed to get SEP-10 challenge: ${challengeRes.status}`);
  }
  const { data: { challenge, networkPassphrase } } = await challengeRes.json();

  // The wallet signs with the passphrase derived from STELLAR_NETWORK; if the
  // server built the challenge for a different network, fail loudly instead
  // of submitting a signature that can never verify.
  const walletPassphrase = getNetworkPassphrase();
  if (networkPassphrase !== walletPassphrase) {
    throw new Error(
      `Network mismatch: server challenge is for "${networkPassphrase}" but this console is configured for "${walletPassphrase}"`,
    );
  }

  const signedChallenge = await signEntityTransaction(challenge);

  const verifyRes = await fetch(`${API_BASE_URL}/stellar/auth`, {
    method: "POST",
    headers: withTraceparent({ "Content-Type": "application/json" }),
    body: JSON.stringify({ signedChallenge }),
  });
  if (!verifyRes.ok) {
    const body = await verifyRes.json().catch(() => ({}));
    throw new Error(
      body.message || `Entity authentication failed: ${verifyRes.status}`,
    );
  }
  const { data: { jwt } } = await verifyRes.json();

  entityToken = jwt;
  return jwt;
}

export function isEntityAuthenticated(): boolean {
  if (!entityToken) return false;
  try {
    const payload = decodeJwtPayload(entityToken);
    if (
      typeof payload.exp === "number" && payload.exp * 1000 < Date.now()
    ) {
      clearEntityAuth();
      return false;
    }
  } catch {
    clearEntityAuth();
    return false;
  }
  return true;
}

/**
 * The authenticated entity's public key — the `sub` claim of the entity JWT,
 * i.e. the identity the server actually verified (not the connected wallet).
 */
export function getEntityJwtSub(): string | null {
  if (!entityToken) return null;
  try {
    const payload = decodeJwtPayload(entityToken);
    return typeof payload.sub === "string" ? payload.sub : null;
  } catch {
    return null;
  }
}

export function clearEntityAuth(): void {
  entityToken = null;
}
