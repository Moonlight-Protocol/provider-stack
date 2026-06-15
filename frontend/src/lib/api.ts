/**
 * API client for the provider-platform dashboard endpoints.
 * Auth follows the exact same pattern as council-console/lib/platform.ts.
 */
import { API_BASE_URL } from "./config.ts";
import { getConnectedAddress, signMessage } from "./wallet.ts";
import { currentTraceparent } from "./tracer.ts";

function withTraceparent(
  headers: Record<string, string>,
): Record<string, string> {
  const tp = currentTraceparent();
  return tp ? { ...headers, traceparent: tp } : headers;
}

const TOKEN_KEY = "console_token";

let authToken: string | null = localStorage.getItem(TOKEN_KEY);

/**
 * Authenticate with provider-platform via challenge-response.
 * The wallet signs the nonce (SEP-53), platform verifies and returns a JWT.
 */
export async function authenticate(): Promise<string> {
  const publicKey = getConnectedAddress();
  if (!publicKey) throw new Error("Wallet not connected");

  const challengeRes = await fetch(`${API_BASE_URL}/dashboard/auth/challenge`, {
    method: "POST",
    headers: withTraceparent({ "Content-Type": "application/json" }),
    body: JSON.stringify({ publicKey }),
  });
  if (!challengeRes.ok) {
    throw new Error(`Failed to get auth challenge: ${challengeRes.status}`);
  }
  const { data: { nonce } } = await challengeRes.json();

  const signature = await signMessage(nonce);

  const verifyRes = await fetch(`${API_BASE_URL}/dashboard/auth/verify`, {
    method: "POST",
    headers: withTraceparent({ "Content-Type": "application/json" }),
    body: JSON.stringify({ nonce, signature, publicKey }),
  });
  if (!verifyRes.ok) {
    throw new Error("Platform authentication failed");
  }
  const { data: { token } } = await verifyRes.json();

  authToken = token;
  localStorage.setItem(TOKEN_KEY, token);
  return token;
}

export function isAuthenticated(): boolean {
  if (!authToken) return false;
  try {
    const b64 = authToken.split(".")[1].replace(/-/g, "+").replace(/_/g, "/");
    const payload = JSON.parse(atob(b64));
    if (payload.exp && payload.exp * 1000 < Date.now()) {
      clearPlatformAuth();
      return false;
    }
  } catch {
    clearPlatformAuth();
    return false;
  }
  return true;
}

export function clearPlatformAuth(): void {
  authToken = null;
  localStorage.removeItem(TOKEN_KEY);
}

async function platformFetch(
  path: string,
  opts: RequestInit = {},
): Promise<Response> {
  if (!authToken) throw new Error("Not authenticated. Please sign in first.");

  const doFetch = () =>
    fetch(`${API_BASE_URL}${path}`, {
      ...opts,
      headers: withTraceparent({
        "Content-Type": "application/json",
        "Authorization": `Bearer ${authToken}`,
        ...(opts.headers as Record<string, string> ?? {}),
      }),
    });

  const res = await doFetch();
  if (res.status === 401) {
    clearPlatformAuth();
    globalThis.location.hash = "#/login";
    throw new Error("Session expired");
  }
  return res;
}

// --- Signing ---

/**
 * Sign a payload with a Stellar secret key (Ed25519).
 * Matches the council-platform's verifyPayload format:
 *   SHA-256(JSON.stringify({ payload, timestamp })) → Ed25519 sign → base64
 */
export async function signPayload<T>(payload: T, secretKey: string): Promise<{
  payload: T;
  signature: string;
  publicKey: string;
  timestamp: number;
}> {
  const { Keypair } = await import("stellar-base");
  const { Buffer } = await import("buffer");
  const keypair = Keypair.fromSecret(secretKey);
  const timestamp = Date.now();
  const canonical = JSON.stringify({ payload, timestamp });
  const hash = new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(canonical)),
  );
  const signature = Buffer.from(keypair.sign(Buffer.from(hash))).toString(
    "base64",
  );
  return { payload, signature, publicKey: keypair.publicKey(), timestamp };
}

// --- PP management ---

export interface ChannelInfo {
  channelContractId: string;
  assetCode: string;
  assetContractId: string;
  label: string | null;
}

export interface MembershipInfo {
  councilUrl: string;
  councilName: string | null;
  status: string;
  channelAuthId: string;
  claimedJurisdictions: string[] | null;
  councilJurisdictions: string[] | null;
  channels: ChannelInfo[];
}

export interface PpInfo {
  publicKey: string;
  derivationIndex: number;
  label: string | null;
  isActive: boolean;
  createdAt: string;
  councilMemberships: MembershipInfo[];
}

export async function getPp(): Promise<PpInfo> {
  const res = await platformFetch("/dashboard/pp");
  if (!res.ok) throw new Error("Failed to fetch provider");
  const { data } = await res.json();
  return data;
}

// --- Council (UC2) ---

export interface CouncilInfo {
  councilUrl: string;
  council: {
    name: string;
    description: string | null;
    contactEmail: string | null;
    channelAuthId: string;
    councilPublicKey: string;
  };
  jurisdictions: Array<{ countryCode: string; label: string | null }>;
  channels: Array<
    { channelContractId: string; assetCode: string; label: string | null }
  >;
  providers: Array<{ publicKey: string; label: string | null }>;
}

export async function discoverCouncil(
  councilUrl: string,
): Promise<CouncilInfo> {
  const res = await platformFetch("/dashboard/council/discover", {
    method: "POST",
    body: JSON.stringify({ councilUrl }),
  });
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.message || `Discovery failed: HTTP ${res.status}`);
  }
  const { data } = await res.json();
  return data;
}

export async function joinCouncil(data: {
  councilUrl: string;
  councilId: string;
  councilName?: string;
  councilPublicKey?: string;
  signedEnvelope: {
    payload: unknown;
    signature: string;
    publicKey: string;
    timestamp: number;
  };
}): Promise<{ joinRequestId: string; status: string }> {
  const res = await platformFetch(
    "/provider/council/join",
    {
      method: "POST",
      body: JSON.stringify(data),
    },
  );
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.message || "Failed to join council");
  }
  const { data: resData } = await res.json();
  return resData;
}

export interface CouncilMembership {
  id: string;
  councilUrl: string;
  councilName: string | null;
  councilPublicKey: string;
  channelAuthId: string;
  status: "PENDING" | "ACTIVE" | "REJECTED";
  config: Record<string, unknown> | null;
  joinRequestId: string | null;
  ppPublicKey: string | null;
  createdAt: string;
}

export async function getCouncilMembership(): Promise<CouncilMembership | null> {
  const res = await platformFetch("/provider/council/membership");
  if (!res.ok) throw new Error("Failed to retrieve membership");
  const { data } = await res.json();
  return data;
}

/**
 * Sync the PP's membership status via the provider-platform.
 * The platform queries the council and updates its local DB.
 * Returns the synced status.
 */
export async function checkMembershipStatus(): Promise<
  "ACTIVE" | "PENDING" | "REJECTED"
> {
  const res = await platformFetch("/provider/council/membership", {
    method: "POST",
    body: JSON.stringify({}),
  });
  if (!res.ok) return "PENDING";
  const { data } = await res.json();
  return data?.status ?? "PENDING";
}

// --- Treasury (for fund check) ---

export interface TreasuryData {
  address: string;
  sequence: string;
  balances: Array<{ asset_type: string; asset_code?: string; balance: string }>;
  lastModifiedLedger: number;
}

export async function getTreasury(): Promise<TreasuryData> {
  const res = await platformFetch("/provider/treasury");
  if (!res.ok) throw new Error("Failed to fetch treasury info");
  const { data } = await res.json();
  return data;
}

// --- Metrics (counter strip + sparklines) ---

export interface MetricsSnapshot {
  recordedAt: string;
  platformVersion: string;
  queueDepth: number;
  slotCount: number;
  bundlesCompleted: number;
  bundlesExpired: number;
  // Optional so the client tolerates a provider-platform release pre-PR-104.
  // The ERROR RATE counter falls back to "—" until every snapshot in the
  // window has this populated.
  bundlesFailed?: number;
  avgProcessingMs: number | null;
  p95ProcessingMs: number | null;
  throughputPerMin: number | null;
}

export interface MetricsResponse {
  rangeMin: number;
  since: string;
  snapshots: MetricsSnapshot[];
}

export async function getMetrics(rangeMin: number): Promise<MetricsResponse> {
  const qs = new URLSearchParams({ rangeMin: String(rangeMin) });
  const res = await platformFetch(`/provider/metrics?${qs}`);
  if (!res.ok) throw new Error("Failed to fetch metrics");
  const body = await res.json();
  return body.data as MetricsResponse;
}

// --- Entities interacting with the provider (operator view) ---

export interface EntityInteraction {
  pubkey: string;
  status: string;
  name: string | null;
  jurisdictions: string[] | null;
  createdAt: string;
  updatedAt: string;
}

export async function getEntities(): Promise<EntityInteraction[]> {
  const res = await platformFetch("/provider/entities");
  if (!res.ok) throw new Error("Failed to fetch entities");
  const { data } = await res.json();
  return data as EntityInteraction[];
}

export type BundleOpKind =
  | "deposit"
  | "withdraw"
  | "spend"
  | "create"
  | "unknown";

export interface BundleOp {
  kind: BundleOpKind;
  address?: string;
  amount?: string;
}

export interface BundleDetail {
  id: string;
  status: string;
  channelContractId: string | null;
  operations: BundleOp[];
  entityName: string | null;
  jurisdictions: string[];
  amount: string | null;
}

export async function getBundleDetail(bundleId: string): Promise<BundleDetail> {
  const res = await platformFetch(
    `/provider/bundles/${encodeURIComponent(bundleId)}`,
  );
  if (!res.ok) throw new Error("Failed to fetch bundle detail");
  const body = await res.json();
  return body.data as BundleDetail;
}

export interface RecentBundleSummary {
  id: string;
  status: "PENDING" | "PROCESSING" | "COMPLETED" | "FAILED" | "EXPIRED";
  channelContractId: string | null;
  entityName: string | null;
  jurisdictions: string[];
  amount: string | null;
  createdAt: string;
  updatedAt: string;
}

export async function listRecentBundles(
  limit: number,
): Promise<RecentBundleSummary[]> {
  const qs = new URLSearchParams({ limit: String(limit) });
  const res = await platformFetch(`/provider/bundles?${qs}`);
  if (!res.ok) throw new Error("Failed to list recent bundles");
  const body = await res.json();
  return (body.data as { bundles: RecentBundleSummary[] }).bundles;
}
