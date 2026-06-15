import { page } from "../components/page.ts";
import { escapeHtml } from "../lib/dom.ts";
import {
  type BundleDetail,
  type EntityInteraction,
  getBundleDetail,
  getEntities,
  getMetrics,
  getPp,
  getTreasury,
  listRecentBundles,
  type MembershipInfo,
  type MetricsSnapshot,
  type PpInfo,
  type RecentBundleSummary,
  type TreasuryData,
} from "../lib/api.ts";
import { onCleanup } from "../lib/router.ts";
import { EventsClient, type ProviderEvent } from "../lib/events-client.ts";
import { getConnectedAddress, signTransaction } from "../lib/wallet.ts";
import { buildFundTx, submitHorizonTx } from "../lib/stellar.ts";
import { API_BASE_URL } from "../lib/config.ts";

// -----------------------------------------------------------------------------
// Shared helpers (unchanged from v1)
// -----------------------------------------------------------------------------

function truncate(s: string, head = 6, tail = 4): string {
  return s.length > head + tail + 1
    ? `${s.slice(0, head)}…${s.slice(-tail)}`
    : s;
}

function flag(code: string): string {
  return code.toUpperCase().replace(
    /./g,
    (c) => String.fromCodePoint(0x1F1E6 + c.charCodeAt(0) - 65),
  );
}

function flags(codes: string[]): string {
  return codes.map((c) =>
    `<span title="${escapeHtml(c)}" style="font-size:1.1rem">${flag(c)}</span>`
  ).join(" ");
}

function ppClaimedJurisdictions(m: MembershipInfo): string[] {
  return (m.claimedJurisdictions ?? []).map((c) => c.toUpperCase());
}

function fmtAmountStroops(stroops: string): string {
  const big = BigInt(stroops);
  const whole = big / 10_000_000n;
  const frac = big % 10_000_000n;
  return `${whole}.${frac.toString().padStart(7, "0").slice(0, 2)}`;
}

function fmtRelativeTime(epochMs: number, now: number): string {
  const delta = Math.max(0, now - epochMs);
  if (delta < 1000) return "just now";
  const sec = Math.floor(delta / 1000);
  if (sec < 60) return `${sec}s ago`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ago`;
  const hr = Math.floor(min / 60);
  return `${hr}h ago`;
}

/** Absolute, human-readable date with hour:minute (24h), e.g. "Jun 12, 2026, 18:16". */
function fmtDateTime(epochMs: number): string {
  return new Date(epochMs).toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

function withBriefCopyFeedback(btn: HTMLElement): void {
  const orig = btn.innerHTML;
  btn.innerHTML =
    `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--active)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>`;
  setTimeout(() => {
    btn.innerHTML = orig;
  }, 1200);
}

/**
 * Pick the membership we'll surface as the primary at-a-glance status: first
 * ACTIVE, else first PENDING, else first row, else null. Single-PP shape — this
 * is the equivalent of the old home-list-row "primary council" summary, now
 * folded into the provider view header.
 */
function primaryMembership(
  memberships: MembershipInfo[],
): MembershipInfo | undefined {
  return memberships.find((m) => m.status === "ACTIVE") ??
    memberships.find((m) => m.status === "PENDING") ??
    memberships[0];
}

function statusBadgeClass(status: string): string {
  if (status === "ACTIVE") return "badge-active";
  if (status === "PENDING") return "badge-pending";
  if (status === "REJECTED") return "badge-inactive";
  return "badge-unverified";
}

// -----------------------------------------------------------------------------
// Top-level view
// -----------------------------------------------------------------------------

async function renderContent(): Promise<HTMLElement> {
  const root = document.createElement("div");

  root.innerHTML =
    `<div style="color:var(--text-muted);margin:2rem 0">Loading provider…</div>`;

  let pp: PpInfo;
  try {
    pp = await getPp();
  } catch (err) {
    root.innerHTML = `<p class="error-text">${
      escapeHtml(err instanceof Error ? err.message : String(err))
    }</p>`;
    return root;
  }

  const memberships = pp.councilMemberships;
  const primary = primaryMembership(memberships);

  let treasury: TreasuryData | null = null;
  try {
    treasury = await getTreasury();
  } catch { /* best effort */ }

  const xlm = treasury?.balances.find((b) => b.asset_type === "native");
  const opexBalance = xlm ? `${parseFloat(xlm.balance).toFixed(2)} XLM` : "—";
  const name = pp.label || truncate(pp.publicKey);

  root.innerHTML = renderTemplate(name, opexBalance, memberships, primary);

  wireHeader(root, pp);
  wireFund(root);
  wireCouncils(root);
  // Best-effort: a fetch failure renders an error row, never blocks the page.
  renderEntitiesSection(root);

  // v2 zones (counter strip / recent bundles / activity feed / sparklines).
  const zones = setupV2Zones({
    root,
    name,
    memberships,
  });

  const client = new EventsClient({
    onEvent: (event) => zones.handleEvent(event),
    onStatus: (status) => zones.setStatus(status),
  });
  client.start();
  onCleanup(() => {
    client.stop();
    zones.stop();
  });

  return root;
}

// -----------------------------------------------------------------------------
// HTML template
//
// v1 top stays AS-IS (header + OpEx card + 3-up Councils list) per `-3` §4.
// v2 zones land in the space the v1 events UI (5-column + mode toggle +
// tx-detail card) used to occupy.
// -----------------------------------------------------------------------------

function renderTemplate(
  name: string,
  opexBalance: string,
  memberships: MembershipInfo[],
  primary: MembershipInfo | undefined,
): string {
  const councilCards = memberships.length === 0
    ? `<div style="color:var(--text-muted)">No council memberships yet.</div>`
    : memberships.map((m) => renderCouncilCard(m)).join("");

  const primaryBadge = primary
    ? `<span class="badge ${statusBadgeClass(primary.status)}" title="${
      escapeHtml(primary.councilName ?? "Council")
    }">${escapeHtml(primary.status)}</span>`
    : `<span class="badge badge-unverified" title="No council memberships">NO COUNCIL</span>`;

  return `
    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:1.5rem">
      <div style="display:flex;align-items:center;gap:0.5rem">
        <h2 style="margin:0">${escapeHtml(name)}</h2>
        ${primaryBadge}
      </div>
      <div style="display:flex;align-items:center;gap:0.25rem">
        <button id="copy-provider-url" class="icon-btn" title="Copy provider URL"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></svg></button>
        <button id="copy-opex-address" class="icon-btn" title="Copy OpEx address"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 7V4a1 1 0 0 0-1-1H5a2 2 0 0 0 0 4h15a1 1 0 0 1 1 1v4h-3a2 2 0 0 0 0 4h3a1 1 0 0 0 1-1v-2"/><path d="M3 5v14a2 2 0 0 0 2 2h15a1 1 0 0 0 1-1v-4"/></svg></button>
        <button id="fund-btn" class="icon-btn" title="Fund OpEx account"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M16 8h-6a2 2 0 0 0 0 4h4a2 2 0 0 1 0 4H8"/><path d="M12 18V6"/></svg></button>
      </div>
    </div>

    <h3 style="margin:0 0 0.5rem">OpEx</h3>
    <div style="padding:0.6rem 0.9rem;margin-bottom:1.5rem;border:1px solid var(--border);border-radius:8px;background:var(--surface);display:inline-flex;flex-direction:column;align-items:flex-start;gap:0.35rem;text-align:left">
      <span style="color:var(--text-muted);font-size:0.7rem;letter-spacing:0.05em;text-transform:uppercase">Balance</span>
      <span style="font-size:1.1rem;font-weight:600">${
    escapeHtml(opexBalance)
  }</span>
    </div>
    <p id="fund-error" class="error-text" hidden style="margin:0 0 1rem"></p>

    <h3 style="margin:0 0 0.5rem">Councils</h3>
    <div id="councils" style="display:grid;grid-template-columns:repeat(3,1fr);gap:0.75rem;margin-bottom:2rem">${councilCards}</div>

    <h3 style="margin:0 0 0.5rem">Dashboard <span style="font-weight:400;font-size:0.78rem;color:var(--text-muted)">(last 6 hours)</span></h3>
    <div class="dashboard-v2" style="display:grid;grid-template-columns:1fr 280px;grid-template-rows:auto 600px auto;grid-template-areas:'counter counter' 'bundles feed' 'sparklines sparklines';gap:0.75rem">
      <div class="zone-counter" style="grid-area:counter;display:grid;grid-template-columns:repeat(4,1fr);gap:0.75rem">
        ${renderCounterBox("throughput", "Throughput", "bundles/min")}
        ${renderCounterBox("latency", "Avg Latency", "ms")}
        ${renderCounterBox("queue", "Queue Peak", "in mempool")}
        ${renderCounterBox("error-rate", "Error Rate", "of bundles")}
      </div>

      <div class="zone-bundles stat-card" style="grid-area:bundles;padding:0.75rem;display:flex;flex-direction:column;min-height:0">
        <div style="display:flex;align-items:baseline;justify-content:space-between;gap:0.75rem;margin-bottom:0.6rem">
          <div style="font-weight:600">Operations</div>
          <div id="preview-bundles-counts" style="font-size:0.78rem;color:var(--text-muted)"></div>
        </div>
        <div id="preview-bundles-table" style="flex:1;overflow:auto;min-height:0"></div>
      </div>

      <div class="zone-feed stat-card" style="grid-area:feed;padding:0.75rem;display:flex;flex-direction:column;min-height:0">
        <div style="font-weight:600;margin-bottom:0.5rem">Live feed</div>
        <div id="activity-feed" style="flex:1;display:flex;flex-direction:column;gap:6px;overflow:hidden"></div>
        <div id="activity-feed-empty" style="flex:1;display:flex;color:var(--text-muted);font-size:0.8rem;text-align:center;padding-top:20px">Nothing happening</div>
      </div>

      <div class="zone-sparklines" style="grid-area:sparklines;display:grid;grid-template-columns:repeat(3,1fr);gap:0.75rem;min-height:180px">
        ${renderSparklineBox("throughput", "Throughput")}
        ${renderSparklineBox("latency", "Latency")}
        ${renderSparklineBox("queue", "Queue depth")}
      </div>
    </div>

    <h3 style="margin:2rem 0 0.5rem">Entities</h3>
    <div id="entities-section" style="margin-bottom:2rem">
      <div id="entities-body" style="color:var(--text-muted);font-size:0.85rem">Loading entities…</div>
    </div>
  `;
}

function renderCounterBox(id: string, label: string, unit: string): string {
  return `
    <div class="zone-counter-box stat-card" style="padding:0.6rem 0.8rem">
      <div style="font-size:0.65rem;letter-spacing:0.06em;text-transform:uppercase;color:var(--text-muted);font-weight:600">${
    escapeHtml(label)
  }</div>
      <div style="display:flex;align-items:baseline;gap:0.3rem;margin-top:0.2rem">
        <span id="counter-${id}-value" style="font-size:1.4rem;font-weight:700;color:var(--text)">—</span>
        <span style="font-size:0.65rem;color:var(--text-muted)">${
    escapeHtml(unit)
  }</span>
      </div>
    </div>
  `;
}

function renderSparklineBox(id: string, label: string): string {
  return `
    <div class="zone-sparkline-box stat-card" style="padding:0.6rem 0.8rem;display:flex;flex-direction:column;gap:0.3rem">
      <div style="font-size:0.7rem;color:var(--text-muted);text-transform:uppercase;letter-spacing:0.04em;margin-bottom:10px">${
    escapeHtml(label)
  }</div>
      <div style="flex:1;display:flex;gap:4px;min-height:120px">
        <div style="display:flex;flex-direction:column;justify-content:space-between;font-size:0.6rem;color:var(--border);text-align:right;min-width:24px;padding:1px 0">
          <span id="sparkline-${id}-max">—</span>
          <span id="sparkline-${id}-min">—</span>
        </div>
        <svg id="sparkline-${id}" viewBox="0 0 ${SPARKLINE_WIDTH} ${SPARKLINE_HEIGHT}" preserveAspectRatio="none" style="flex:1;width:100%;height:auto">
          <line x1="0" y1="0" x2="0" y2="${SPARKLINE_HEIGHT}" stroke="var(--border)" stroke-width="1" vector-effect="non-scaling-stroke"></line>
          <line x1="0" y1="${SPARKLINE_HEIGHT}" x2="${SPARKLINE_WIDTH}" y2="${SPARKLINE_HEIGHT}" stroke="var(--border)" stroke-width="1" vector-effect="non-scaling-stroke"></line>
          <polyline fill="none" stroke="${SPARKLINE_COLOR}" stroke-width="1.5" vector-effect="non-scaling-stroke" points=""></polyline>
          <text x="${SPARKLINE_WIDTH / 2}" y="${
    SPARKLINE_HEIGHT / 2
  }" text-anchor="middle" dominant-baseline="middle" fill="var(--text-muted)" font-size="10" id="sparkline-${id}-empty">—</text>
        </svg>
      </div>
      <div style="display:flex;justify-content:space-between;font-size:0.6rem;color:var(--border);padding-left:28px">
        <span>${Math.floor(SPARKLINE_RANGE_MIN / 60)}h ago</span>
        <span>now</span>
      </div>
    </div>
  `;
}

function renderCouncilCard(m: MembershipInfo): string {
  const claimed = ppClaimedJurisdictions(m);
  const flagsHtml = claimed.length ? flags(claimed) : "—";
  const assetChips = m.channels.length
    ? m.channels.map((c) =>
      `<span class="badge badge-active" style="margin-right:0.25rem">${
        escapeHtml(c.assetCode)
      }</span>`
    ).join("")
    : '<span style="color:var(--text-muted);font-size:0.85rem">No assets yet</span>';
  return `
    <div class="stat-card" style="padding:0.75rem 1rem">
      <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:0.4rem">
        <span style="font-weight:600">${escapeHtml(m.councilName || "—")}</span>
        <div>${flagsHtml}</div>
      </div>
      <div style="display:flex;gap:0.25rem;flex-wrap:wrap">${assetChips}</div>
    </div>
  `;
}

// -----------------------------------------------------------------------------
// Entities section — every pubkey that has interacted with this PP
// -----------------------------------------------------------------------------

/** Maps a per-PP entity status to one of the lib's badge variants. UNVERIFIED
 * uses a neutral local variant (badge-unverified, app-styles.css). */
function entityStatusBadge(status: string): string {
  const cls = status === "APPROVED"
    ? "badge-active"
    : status === "PENDING"
    ? "badge-pending"
    : status === "BLOCKED"
    ? "badge-inactive"
    : "badge-unverified"; // UNVERIFIED + any unknown future value
  return `<span class="badge ${cls}">${escapeHtml(status)}</span>`;
}

const ENTITY_COPY_ICON =
  `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></svg>`;

async function renderEntitiesSection(root: HTMLElement): Promise<void> {
  const body = root.querySelector("#entities-body") as HTMLElement | null;
  if (!body) return;

  let entities: EntityInteraction[];
  try {
    entities = await getEntities();
  } catch (err) {
    body.innerHTML = `<span class="error-text">${
      escapeHtml(
        err instanceof Error ? err.message : "Failed to load entities",
      )
    }</span>`;
    return;
  }

  if (entities.length === 0) {
    body.textContent = "No entities have interacted with this provider yet";
    return;
  }

  const rows = entities.map((e) => {
    const date = fmtDateTime(new Date(e.updatedAt).getTime());
    return `
      <tr>
        <td style="white-space:nowrap;color:var(--text-muted)">${
      escapeHtml(date)
    }</td>
        <td>
          <span style="font-family:monospace">${
      escapeHtml(truncate(e.pubkey))
    }</span>
          <button class="icon-btn entity-copy-btn" data-pubkey="${
      escapeHtml(e.pubkey)
    }" title="Copy public key" style="vertical-align:middle">${ENTITY_COPY_ICON}</button>
        </td>
        <td>${entityStatusBadge(e.status)}</td>
      </tr>`;
  }).join("");

  body.innerHTML = `
    <table style="margin:0">
      <thead>
        <tr><th>Date</th><th>Public Key</th><th>Status</th></tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;

  body.querySelectorAll(".entity-copy-btn").forEach((el) => {
    const btn = el as HTMLButtonElement;
    btn.addEventListener("click", () => {
      const pubkey = btn.dataset.pubkey;
      if (!pubkey) return;
      navigator.clipboard.writeText(pubkey).then(() =>
        withBriefCopyFeedback(btn)
      );
    });
  });
}

// -----------------------------------------------------------------------------
// v1 header / fund / councils wiring (unchanged paths)
// -----------------------------------------------------------------------------

function wireHeader(root: HTMLElement, pp: PpInfo): void {
  const opexBtn = root.querySelector(
    "#copy-opex-address",
  ) as HTMLButtonElement | null;
  opexBtn?.addEventListener("click", () => {
    navigator.clipboard.writeText(pp.publicKey).then(() =>
      withBriefCopyFeedback(opexBtn)
    );
  });
  const urlBtn = root.querySelector(
    "#copy-provider-url",
  ) as HTMLButtonElement | null;
  urlBtn?.addEventListener("click", () => {
    const providerUrl = new URL(API_BASE_URL).origin;
    navigator.clipboard.writeText(providerUrl).then(() =>
      withBriefCopyFeedback(urlBtn)
    );
  });
}

function wireFund(root: HTMLElement): void {
  const fundBtn = root.querySelector("#fund-btn") as HTMLButtonElement;
  const errEl = root.querySelector("#fund-error") as HTMLElement;
  fundBtn?.addEventListener("click", async () => {
    const amount = globalThis.prompt(
      "Amount in XLM to send from your wallet to the provider's OpEx address:",
      "10",
    );
    if (!amount) return;
    fundBtn.disabled = true;
    errEl.hidden = true;
    try {
      const pp = await getPp();
      const ppPublicKey = pp.publicKey;
      const source = getConnectedAddress();
      if (!source) throw new Error("Wallet not connected");
      const xdr = await buildFundTx(source, ppPublicKey, amount.trim());
      console.debug("[fund] built tx xdr (first 80 chars)", xdr.slice(0, 80));
      const signed = await signTransaction(xdr);
      console.debug("[fund] signed xdr (first 80 chars)", signed.slice(0, 80));
      await submitHorizonTx(signed);
      console.debug("[fund] submitted OK");
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("[fund] failed", err);
      errEl.textContent = msg;
      errEl.hidden = false;
    } finally {
      fundBtn.disabled = false;
    }
  });
}

function wireCouncils(_root: HTMLElement): void {
  // v1 had asset-chip click-to-copy on the 3-up council cards. Dropped per
  // `-3` §5 ("Asset-chip-copy — PM confirmed drop; do not restore.").
  // The 3-up render itself stays per `-3` §4.
}

// -----------------------------------------------------------------------------
// v2 zones — counter strip + recent bundles + activity feed + sparklines
// All zones are always-live: no Range mode, no Search, no mode toggle.
// -----------------------------------------------------------------------------

const SPARKLINE_WIDTH = 300;
const SPARKLINE_HEIGHT = 80;
const SPARKLINE_COLOR = "#1c7ed6";

const METRICS_POLL_MS = 60_000;
const SPARKLINE_RANGE_MIN = 360;
const FEED_MAX_CARDS = 5;
const FEED_CARD_LIFETIME_MS = 8_000;
const FEED_CARD_FADE_MS = 300;

// Event colors mirror the table's stage colors so the visual language is
// consistent (queued amber, submitting blue, completed green, failed red,
// expired gray). Non-bundle events (channel joins/leaves) stay neutral gray.
const FEED_COLORS: Record<ProviderEvent["kind"], string> = {
  "mempool.bundle_added": "#f59f00",
  "mempool.bundle_expired": "#868e96",
  "executor.transaction_submitted": "#1c7ed6",
  "executor.execution_failed": "#e03131",
  "verifier.bundle_completed": "#2f9e44",
  "verifier.bundle_failed": "#e03131",
  "bundle.deposit_completed": "#2f9e44",
  "bundle.withdraw_completed": "#2f9e44",
  "channel.provider_added": "#868e96",
  "channel.provider_removed": "#868e96",
};

const FEED_LABELS: Record<ProviderEvent["kind"], string> = {
  "bundle.deposit_completed": "Deposit",
  "mempool.bundle_added": "Mempool",
  "mempool.bundle_expired": "Expired",
  "executor.transaction_submitted": "Submitted",
  "executor.execution_failed": "Execution failed",
  "verifier.bundle_completed": "Verified",
  "verifier.bundle_failed": "Verify failed",
  "bundle.withdraw_completed": "Withdraw",
  "channel.provider_added": "Channel joined",
  "channel.provider_removed": "Channel left",
};

interface SetupOpts {
  root: HTMLElement;
  name: string;
  memberships: MembershipInfo[];
}

interface ZoneHandle {
  handleEvent: (event: ProviderEvent) => void;
  setStatus: (status: "connecting" | "open" | "closed") => void;
  stop: () => void;
}

function setupV2Zones(opts: SetupOpts): ZoneHandle {
  const { root, memberships } = opts;

  const feedEl = root.querySelector("#activity-feed") as HTMLElement | null;
  const feedEmptyEl = root.querySelector(
    "#activity-feed-empty",
  ) as HTMLElement | null;

  // channelContractId → council name, for resolving feed-card subtitles and
  // recent-bundles table labels. Stable for the view's lifetime.
  const channelToCouncil = new Map<
    string,
    {
      index: number;
      councilName: string | null;
      assetCode: string;
    }
  >();
  memberships.forEach((m, i) => {
    for (const ch of m.channels) {
      channelToCouncil.set(ch.channelContractId, {
        index: i,
        councilName: m.councilName,
        assetCode: ch.assetCode,
      });
    }
  });

  function pushFeedCard(
    event: ProviderEvent,
    opts: { persistent?: boolean } = {},
  ): void {
    if (!feedEl) return;
    const color = FEED_COLORS[event.kind];
    const kindLabel = FEED_LABELS[event.kind];

    let amountHtml = "";
    if (event.kind === "bundle.deposit_completed") {
      amountHtml = `<span style="font-size:0.7rem;color:#333">${
        fmtAmountStroops(event.payload.amount)
      } XLM</span>`;
    } else if (event.kind === "bundle.withdraw_completed") {
      amountHtml = `<span style="font-size:0.7rem;color:#333">${
        fmtAmountStroops(event.payload.amount)
      } XLM</span>`;
    }

    const ts = Date.now();
    const card = document.createElement("div");
    card.className = "activity-feed-card";
    card.dataset.ts = String(ts);
    card.style.cssText =
      `border-left:1px solid ${color};background:var(--surface);padding:0.4rem 0.55rem;font-size:0.8rem;opacity:0;transition:opacity ${FEED_CARD_FADE_MS}ms`;
    card.innerHTML = `
      <div style="display:flex;align-items:center;gap:0.5rem">
        <span style="font-weight:600">${escapeHtml(kindLabel)}</span>
        ${amountHtml}
        <span class="activity-feed-relative" style="margin-left:auto;color:#555;font-size:0.7rem">just now</span>
      </div>
    `;
    feedEl.prepend(card);
    feedEl.style.display = "flex";
    if (feedEmptyEl) feedEmptyEl.style.display = "none";

    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        card.style.opacity = "1";
      });
    });

    while (feedEl.childElementCount > FEED_MAX_CARDS) {
      feedEl.lastElementChild?.remove();
    }

    if (opts.persistent) return;

    setTimeout(() => {
      card.style.opacity = "0";
      setTimeout(() => {
        card.remove();
        if (feedEl.childElementCount === 0 && feedEmptyEl) {
          feedEl.style.display = "none";
          feedEmptyEl.style.display = "flex";
        }
      }, FEED_CARD_FADE_MS);
    }, FEED_CARD_LIFETIME_MS);
  }

  // On mount, the feed has no children — hide it so the empty placeholder
  // (which has flex:1) takes the full feed area and centers properly.
  if (feedEl) feedEl.style.display = "none";

  // Counter strip + sparklines: /dashboard/metrics polled every 60s (same
  // cadence as the platform's MetricsCollector snapshot loop).

  function applyMetrics(resp: { snapshots: MetricsSnapshot[] }): void {
    const snapshots = resp.snapshots;

    // QUEUE DEPTH (peak over the 6h window): the 60s collector snapshots a
    // point-in-time mempool count, so a fast-draining pipeline often reads
    // zero between cycles. Peak preserves the worst congestion seen.
    const queueValues = snapshots
      .map((s) => s.queueDepth)
      .filter((v): v is number => typeof v === "number");
    const peakQueue = queueValues.length > 0 ? Math.max(...queueValues) : null;
    setCounter("queue", peakQueue, (v) => String(v));

    // THROUGHPUT (last 6h): mean of `throughputPerMin` across all snapshots
    // in the window.
    const throughputValues = snapshots
      .map((s) => s.throughputPerMin)
      .filter((v): v is number => typeof v === "number");
    const meanThroughput = throughputValues.length > 0
      ? throughputValues.reduce((a, b) => a + b, 0) / throughputValues.length
      : null;
    setCounter("throughput", meanThroughput, (v) => v.toFixed(2));

    // AVG LATENCY (last 6h): weighted avg of avgProcessingMs across every
    // snapshot in the window, weighted by bundlesCompleted.
    let weightSum = 0;
    let weightedLatency = 0;
    for (const s of snapshots) {
      if (s.avgProcessingMs == null) continue;
      weightedLatency += s.avgProcessingMs * s.bundlesCompleted;
      weightSum += s.bundlesCompleted;
    }
    const avgLatency = weightSum > 0 ? weightedLatency / weightSum : null;
    setCounter("latency", avgLatency, (v) => v.toFixed(0));

    // ERROR RATE (last 6h): bundlesFailed / (bundlesCompleted + bundlesFailed +
    // bundlesExpired). Requires bundlesFailed on every snapshot — pre-PR-104
    // platforms omit the field, in which case we show "—" rather than a
    // misleading 0%.
    const hasFailureData = snapshots.length > 0 &&
      snapshots.every((s) => typeof s.bundlesFailed === "number");
    if (hasFailureData) {
      let failed = 0;
      let terminal = 0;
      for (const s of snapshots) {
        failed += s.bundlesFailed ?? 0;
        terminal += (s.bundlesFailed ?? 0) + s.bundlesCompleted +
          s.bundlesExpired;
      }
      const rate = terminal > 0 ? (failed / terminal) * 100 : 0;
      setCounter("error-rate", rate, (v) => `${v.toFixed(1)}%`);
    } else {
      setCounter("error-rate", null, () => "—");
    }

    drawSparkline(
      "throughput",
      snapshots,
      (s) => s.throughputPerMin,
    );
    drawSparkline(
      "latency",
      snapshots,
      (s) => s.avgProcessingMs == null ? null : s.avgProcessingMs / 1000,
    );
    drawSparkline("queue", snapshots, (s) => s.queueDepth);
  }

  function setCounter(
    id: string,
    value: number | null,
    fmt: (v: number) => string,
  ): void {
    const el = root.querySelector(
      `#counter-${id}-value`,
    ) as HTMLElement | null;
    if (!el) return;
    el.textContent = value == null ? "—" : fmt(value);
  }

  function drawSparkline(
    id: string,
    snapshots: MetricsSnapshot[],
    pick: (s: MetricsSnapshot) => number | null,
  ): void {
    const svgEl = root.querySelector(
      `#sparkline-${id}`,
    ) as SVGSVGElement | null;
    if (!svgEl) return;
    const polyline = svgEl.querySelector("polyline");
    const empty = svgEl.querySelector(`#sparkline-${id}-empty`) as
      | SVGTextElement
      | null;
    const maxLabel = root.querySelector(
      `#sparkline-${id}-max`,
    ) as HTMLElement | null;
    const minLabel = root.querySelector(
      `#sparkline-${id}-min`,
    ) as HTMLElement | null;
    if (!polyline) return;

    // Server returns newest→oldest; reverse so x grows with time.
    const ordered = [...snapshots].reverse();
    const values = ordered.map(pick);

    if (values.every((v) => v == null)) {
      polyline.setAttribute("points", "");
      if (empty) empty.style.display = "";
      if (maxLabel) maxLabel.textContent = "—";
      if (minLabel) minLabel.textContent = "—";
      return;
    }
    if (empty) empty.style.display = "none";

    const finiteValues = values.filter((v): v is number => v != null);
    const minV = Math.min(...finiteValues, 0);
    const maxV = Math.max(...finiteValues, minV + 1);
    const range = maxV - minV;
    const n = values.length;
    const pairs: Array<{ x: number; y: number }> = [];
    values.forEach((v, i) => {
      if (v == null) return;
      const x = n === 1 ? SPARKLINE_WIDTH / 2 : (i / (n - 1)) * SPARKLINE_WIDTH;
      const yNorm = range === 0 ? 0.5 : (v - minV) / range;
      const y = SPARKLINE_HEIGHT - yNorm * SPARKLINE_HEIGHT;
      pairs.push({ x, y });
    });

    polyline.setAttribute(
      "points",
      pairs.map((p) => `${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(" "),
    );
    if (maxLabel) maxLabel.textContent = fmtAxisValue(maxV);
    if (minLabel) minLabel.textContent = fmtAxisValue(minV);
  }

  function fmtAxisValue(v: number): string {
    if (!Number.isFinite(v)) return "—";
    if (v === 0) return "0";
    const abs = Math.abs(v);
    if (abs >= 100) return v.toFixed(0);
    if (abs >= 10) return v.toFixed(1);
    return v.toFixed(2);
  }

  // Refresh activity-feed cards' "Xs ago" subtitle so the relative time stays
  // current between events.
  const tickerInterval = globalThis.setInterval(() => {
    const now = Date.now();
    root.querySelectorAll(".activity-feed-card").forEach((card) => {
      const ts = Number((card as HTMLElement).dataset.ts ?? 0);
      const rel = card.querySelector(".activity-feed-relative");
      if (rel && ts) rel.textContent = fmtRelativeTime(ts, now);
    });
  }, 1000);

  let metricsTimer: number | null = null;
  let stopped = false;

  async function pollMetrics(): Promise<void> {
    if (stopped) return;
    try {
      const data = await getMetrics(SPARKLINE_RANGE_MIN);
      if (!stopped) applyMetrics(data);
    } catch (err) {
      console.warn("[v2-zones] metrics poll failed", err);
    } finally {
      if (!stopped) {
        metricsTimer = globalThis.setTimeout(
          pollMetrics,
          METRICS_POLL_MS,
        ) as unknown as number;
      }
    }
  }
  void pollMetrics();

  // -------------------------------------------------------------------------
  // Preview sections (A / B / C) — temporary spike to compare layouts.
  // -------------------------------------------------------------------------
  type PreviewStage =
    | "queued"
    | "submitting"
    | "completed"
    | "failed"
    | "expired";

  interface PreviewBundle {
    bundleId: string;
    channelContractId: string | null;
    jurisdictions: string[];
    entityName: string | null;
    amount: string | null;
    stage: PreviewStage;
    firstSeenTs: number;
    lastUpdateTs: number;
  }

  const PREVIEW_TABLE_LIMIT = 100;
  const PREVIEW_STAGE_ORDER: PreviewStage[] = [
    "queued",
    "submitting",
    "completed",
    "failed",
    "expired",
  ];
  const PREVIEW_STAGE_BORDERS: Record<PreviewStage, string> = {
    queued: "#f59f00",
    submitting: "#1c7ed6",
    completed: "#2f9e44",
    failed: "#e03131",
    expired: "#868e96",
  };
  const previewBundles = new Map<string, PreviewBundle>();
  const bundleDetails = new Map<string, BundleDetail | "loading" | "error">();

  function previewEnsureDetail(bundleId: string): void {
    if (bundleDetails.has(bundleId)) return;
    bundleDetails.set(bundleId, "loading");
    getBundleDetail(bundleId).then(
      (d) => {
        bundleDetails.set(bundleId, d);
        // Enrich the row with entity data fetched from the detail endpoint —
        // covers the case where the live WS event arrived without it.
        const existing = previewBundles.get(bundleId);
        if (existing) {
          if (d.entityName && !existing.entityName) {
            existing.entityName = d.entityName;
          }
          if (
            d.jurisdictions.length > 0 && existing.jurisdictions.length === 0
          ) {
            existing.jurisdictions = d.jurisdictions;
          }
          if (d.amount && !existing.amount) {
            existing.amount = d.amount;
          }
        }
        renderPreviewSections();
      },
      () => {
        bundleDetails.set(bundleId, "error");
        renderPreviewSections();
      },
    );
  }

  function previewUpsert(
    bundleId: string,
    stage: PreviewStage,
    channelContractId: string | null,
    ts: number,
    jurisdictions: string[] = [],
    entityName: string | null = null,
    amount: string | null = null,
  ): void {
    const existing = previewBundles.get(bundleId);
    if (existing) {
      existing.stage = stage;
      existing.lastUpdateTs = ts;
      if (channelContractId && !existing.channelContractId) {
        existing.channelContractId = channelContractId;
      }
      if (jurisdictions.length > 0 && existing.jurisdictions.length === 0) {
        existing.jurisdictions = jurisdictions;
      }
      if (entityName && !existing.entityName) {
        existing.entityName = entityName;
      }
      if (amount && !existing.amount) {
        existing.amount = amount;
      }
      return;
    }
    previewBundles.set(bundleId, {
      bundleId,
      channelContractId,
      jurisdictions,
      entityName,
      amount,
      stage,
      firstSeenTs: ts,
      lastUpdateTs: ts,
    });
    previewEnsureDetail(bundleId);
  }

  function statusToStage(
    status: RecentBundleSummary["status"],
  ): PreviewStage {
    switch (status) {
      case "PENDING":
        return "queued";
      case "PROCESSING":
        return "submitting";
      case "COMPLETED":
        return "completed";
      case "FAILED":
        return "failed";
      case "EXPIRED":
        return "expired";
    }
  }

  async function previewLoadHistorical(): Promise<void> {
    try {
      const recent = await listRecentBundles(PREVIEW_TABLE_LIMIT);
      for (const r of recent) {
        const ts = new Date(r.updatedAt).getTime();
        previewUpsert(
          r.id,
          statusToStage(r.status),
          r.channelContractId,
          ts,
          r.jurisdictions,
          r.entityName,
          r.amount,
        );
      }
      renderPreviewSections();
    } catch (err) {
      console.warn("[v2-zones] historical bundle fetch failed", err);
    }
  }

  function previewIngest(event: ProviderEvent): void {
    switch (event.kind) {
      case "mempool.bundle_added":
        previewUpsert(
          event.payload.bundleId,
          "queued",
          event.payload.channelContractId,
          event.ts,
          event.payload.jurisdictions,
          event.payload.entityName,
          event.payload.amount,
        );
        return;
      case "mempool.bundle_expired":
        previewUpsert(
          event.payload.bundleId,
          "expired",
          event.payload.channelContractId,
          event.ts,
        );
        return;
      case "executor.transaction_submitted":
        for (const id of event.payload.bundleIds) {
          previewUpsert(
            id,
            "submitting",
            event.payload.channelContractId,
            event.ts,
          );
        }
        return;
      case "executor.execution_failed":
        for (const id of event.payload.bundleIds) {
          previewUpsert(
            id,
            "failed",
            event.payload.channelContractId,
            event.ts,
          );
        }
        return;
      case "verifier.bundle_completed":
        for (const id of event.payload.bundleIds) {
          previewUpsert(
            id,
            "completed",
            event.payload.channelContractId,
            event.ts,
          );
        }
        return;
      case "verifier.bundle_failed":
        for (const id of event.payload.bundleIds) {
          previewUpsert(
            id,
            "failed",
            event.payload.channelContractId,
            event.ts,
          );
        }
        return;
      case "bundle.deposit_completed":
      case "bundle.withdraw_completed":
        previewUpsert(
          event.payload.bundleId,
          "completed",
          event.payload.channelContractId,
          event.ts,
        );
        return;
      case "channel.provider_added":
      case "channel.provider_removed":
        return;
    }
  }

  function capitalize(s: string): string {
    return s.length === 0 ? s : s[0].toUpperCase() + s.slice(1);
  }

  function computeActionLabel(
    bundleId: string,
  ): { label: string; tooltip: string } {
    const detail = bundleDetails.get(bundleId);
    if (detail === undefined || detail === "loading") {
      return { label: "—", tooltip: "Loading actions…" };
    }
    if (detail === "error") {
      return { label: "—", tooltip: "Failed to load actions" };
    }
    if (detail.operations.length === 0) {
      return { label: "—", tooltip: "No actions" };
    }
    const hasDeposit = detail.operations.some((o) => o.kind === "deposit");
    const hasWithdraw = detail.operations.some((o) => o.kind === "withdraw");
    const label = hasDeposit ? "Deposit" : hasWithdraw ? "Withdraw" : "Send";
    const tooltip = "Actions: " +
      detail.operations.map((o) => capitalize(o.kind)).join(", ");
    return { label, tooltip };
  }

  function renderPreviewSections(): void {
    const now = Date.now();
    const all = [...previewBundles.values()];

    // --- Section A: Recent bundles table ------------------------------------
    const tableEl = root.querySelector(
      "#preview-bundles-table",
    ) as HTMLElement | null;
    if (tableEl) {
      const recent = [...all].sort((a, b) => b.lastUpdateTs - a.lastUpdateTs)
        .slice(0, PREVIEW_TABLE_LIMIT);
      const tableHeader = `
        <thead>
          <tr style="text-align:left;color:var(--text-muted);font-size:0.7rem;text-transform:uppercase">
            <th style="padding:0.25rem 0.5rem;font-weight:500;position:sticky;top:0;background:var(--surface);border-bottom:1px solid var(--border);z-index:1">Entity</th>
            <th style="padding:0.25rem 0.5rem;font-weight:500;min-width:12%;text-align:center;position:sticky;top:0;background:var(--surface);border-bottom:1px solid var(--border);z-index:1">Action</th>
            <th style="padding:0.25rem 0.5rem;font-weight:500;min-width:12%;text-align:center;position:sticky;top:0;background:var(--surface);border-bottom:1px solid var(--border);z-index:1">Jurisdiction</th>
            <th style="padding:0.25rem 0.5rem;font-weight:500;min-width:12%;text-align:center;position:sticky;top:0;background:var(--surface);border-bottom:1px solid var(--border);z-index:1">Amount</th>
            <th style="padding:0.25rem 0.5rem;font-weight:500;min-width:12%;text-align:center;position:sticky;top:0;background:var(--surface);border-bottom:1px solid var(--border);z-index:1">Asset</th>
            <th style="padding:0.25rem 0.5rem;font-weight:500;min-width:12%;text-align:center;position:sticky;top:0;background:var(--surface);border-bottom:1px solid var(--border);z-index:1">Date</th>
          </tr>
        </thead>`;
      if (recent.length === 0) {
        tableEl.innerHTML = `
          <div style="display:flex;flex-direction:column;height:100%">
            <table style="width:100%;border-collapse:collapse;font-size:0.85rem">
              ${tableHeader}
            </table>
            <div style="flex:1;display:flex;align-items:center;justify-content:center;color:var(--text-muted)">No Operations</div>
          </div>`;
      } else {
        const rows = recent.map((b) => {
          const matched = b.channelContractId
            ? channelToCouncil.get(b.channelContractId)
            : undefined;
          const assetLabel = matched?.assetCode
            ? escapeHtml(matched.assetCode)
            : "—";
          const jurisdictionLabel = b.jurisdictions.length === 0
            ? `<span style="color:var(--text-muted)">—</span>`
            : b.jurisdictions
              .map((j) => `<span title="${escapeHtml(j)}">${flag(j)}</span>`)
              .join(" ");
          const stageColor = PREVIEW_STAGE_BORDERS[b.stage];
          const entityLabel = b.entityName
            ? escapeHtml(b.entityName)
            : `<span style="color:var(--text-muted)">—</span>`;
          const action = computeActionLabel(b.bundleId);
          const amountLabel = b.amount
            ? escapeHtml(fmtAmountStroops(b.amount))
            : `<span style="color:var(--text-muted)">—</span>`;
          return `
            <tr>
              <td style="padding:0.25rem 0.5rem;font-size:0.78rem"><div title="${
            b.stage.charAt(0).toUpperCase() + b.stage.slice(1)
          }" style="display:flex;align-items:flex-end;gap:0.5rem;cursor:default"><span style="display:inline-block;width:5px;height:5px;border-radius:50%;background:${stageColor};align-self:center;flex:0 0 auto"></span><span>${entityLabel}</span></div></td>
              <td style="padding:0.25rem 0.5rem;font-size:0.78rem;min-width:12%;text-align:center"><span title="${
            escapeHtml(action.tooltip)
          }">${escapeHtml(action.label)}</span></td>
              <td style="padding:0.25rem 0.5rem;font-size:0.85rem;min-width:12%;text-align:center">${jurisdictionLabel}</td>
              <td style="padding:0.25rem 0.5rem;font-size:0.78rem;min-width:12%;text-align:center">${amountLabel}</td>
              <td style="padding:0.25rem 0.5rem;font-size:0.78rem;min-width:12%;text-align:center">${assetLabel}</td>
              <td style="padding:0.25rem 0.5rem;color:var(--text-muted);font-size:0.75rem;min-width:12%;text-align:center">${
            fmtRelativeTime(b.lastUpdateTs, now)
          }</td>
            </tr>`;
        }).join("");
        tableEl.innerHTML = `
          <table style="width:100%;border-collapse:collapse;font-size:0.85rem">
            ${tableHeader}
            <tbody>${rows}</tbody>
          </table>`;
      }
    }

    // --- Counts strip (above the table) -------------------------------------
    const countsEl = root.querySelector(
      "#preview-bundles-counts",
    ) as HTMLElement | null;
    if (countsEl) {
      const counts: Record<PreviewStage, number> = {
        queued: 0,
        submitting: 0,
        completed: 0,
        failed: 0,
        expired: 0,
      };
      for (const b of all) counts[b.stage]++;
      countsEl.textContent = PREVIEW_STAGE_ORDER
        .map((s) => `${counts[s]} ${s}`)
        .join(" · ");
    }
  }

  renderPreviewSections();
  void previewLoadHistorical();

  return {
    handleEvent(event) {
      pushFeedCard(event);
      previewIngest(event);
      renderPreviewSections();
    },
    setStatus(_status) {
      // Always-live: no Range fallback. EventsClient handles WS reconnect
      // exponentially; the UI doesn't flip modes.
    },
    stop() {
      stopped = true;
      if (metricsTimer !== null) globalThis.clearTimeout(metricsTimer);
      globalThis.clearInterval(tickerInterval);
    },
  };
}

export const providerView = page(renderContent);
