/**
 * Entity UTXO payment surface at #/pay-utxo.
 *
 * Login: entity SEP-10 against GET/POST /stellar/auth — connect + sign,
 * nothing else. A separate name-based surface lands later at #/pay-name.
 *
 * Signed in, three sections:
 *   Balance   — session balance + deposit into the channel (one wallet popup)
 *   Request   — generate receiving keys, share the CREATE MLXDRs out of band
 *   Send      — paste receiver MLXDRs, spend session UTXOs (no popup)
 *
 * Isolation contract — same stance as the KYC route (entities/register.ts):
 *   - ZERO persistence: wallet address, entity JWT, and all UTXO keys live
 *     in module-local state only; nothing survives refresh or re-entry.
 *   - No session bleed: never reads or writes the operator-auth storage.
 *   - No operator chrome.
 *
 * The channel is provided via query params (#/pay-utxo?channel=C…&asset=C…)
 * — there is no entity-facing channel-discovery endpoint.
 */
import {
  authenticateEntity,
  clearEntityAuth,
  getEntityJwtSub,
} from "../lib/entity-auth.ts";
import {
  clearEntityWallet,
  connectEntityWallet,
  getEntityAddress,
} from "../lib/wallet-entity.ts";
import {
  type BundleState,
  type ChannelConfig,
  DEPOSIT_FEE,
  fromStroops,
  getBundle,
  parseReceiverOps,
  prepareReceive,
  SEND_FEE,
  submitDeposit,
  submitSend,
  toStroops,
} from "../lib/moonlight-client.ts";
import {
  balance,
  clearUtxoSession,
  fundedUtxos,
  pendingAmount,
} from "../lib/utxo-session.ts";
import { capture } from "../lib/analytics.ts";
import { escapeHtml } from "../lib/dom.ts";
import { getRouteQuery, onCleanup } from "../lib/router.ts";
import { startTrace, withSpan } from "../lib/tracer.ts";

// ── helpers ────────────────────────────────────────────────────

function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 4)}…${id.slice(-4)}` : id;
}

function readChannelConfig(): ChannelConfig | null {
  const q = getRouteQuery();
  const channel = q.get("channel");
  const asset = q.get("asset");
  if (!channel || !asset) return null;
  return { channelContractId: channel, assetContractId: asset };
}

/** ui-stepper (Submitted → Processing → Completed) for a bundle status. */
function stepperHtml(state: BundleState | null, error?: string): string {
  if (error) {
    return `<p class="error-text" style="margin-top:1rem">${
      escapeHtml(error)
    }</p>`;
  }
  if (!state) return "";
  const failed = state.status === "FAILED" || state.status === "EXPIRED";
  const step2 = state.status === "PENDING" || state.status === "PROCESSING"
    ? "active"
    : "done";
  const step3 = state.status === "COMPLETED" ? "done" : "";
  const detail = failed && state.failureDetail
    ? `<p class="error-text mono" style="font-size:0.7rem;word-break:break-all">${
      escapeHtml(JSON.stringify(state.failureDetail))
    }</p>`
    : "";
  if (failed) {
    return `
      <p class="error-text" style="margin-top:1rem">Bundle ${state.status}</p>
      ${detail}
      <p class="mono" style="font-size:0.7rem;color:var(--text-muted)">bundle ${
      escapeHtml(state.id)
    }</p>`;
  }
  return `
    <div class="onboarding-stepper" style="margin:1rem 0 0;padding:0.25rem 0 0">
      <div class="onboarding-step done">
        <div class="step-dot">✓</div><div class="step-label">Submitted</div>
      </div>
      <div class="step-line ${step2 === "done" ? "done" : ""}"></div>
      <div class="onboarding-step ${step2}">
        <div class="step-dot">2</div><div class="step-label">Processing</div>
      </div>
      <div class="step-line ${step3 ? "done" : ""}"></div>
      <div class="onboarding-step ${step3}">
        <div class="step-dot">${
    step3 ? "✓" : "3"
  }</div><div class="step-label">Completed</div>
      </div>
    </div>
    <p class="mono" style="text-align:right;font-size:0.7rem;color:var(--text-muted)">bundle ${
    escapeHtml(state.id)
  }</p>`;
}

/** Poll a bundle to a terminal state, invoking onUpdate on each change. */
function pollBundle(
  bundleId: string,
  onUpdate: (state: BundleState) => void,
  onError: (err: string) => void,
): () => void {
  let stopped = false;
  const tick = async () => {
    if (stopped) return;
    try {
      const state = await getBundle(bundleId);
      onUpdate(state);
      if (
        state.status === "COMPLETED" || state.status === "FAILED" ||
        state.status === "EXPIRED"
      ) return;
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
      return;
    }
    setTimeout(tick, 2000);
  };
  tick();
  return () => {
    stopped = true;
  };
}

// ── authenticated surface ──────────────────────────────────────

function paySurface(): HTMLElement {
  const sub = getEntityJwtSub();
  const channel = readChannelConfig();

  const container = document.createElement("div");
  container.className = "container";
  container.style.maxWidth = "680px";
  container.innerHTML = `
    <div style="display:flex;align-items:center;justify-content:space-between;gap:1rem;flex-wrap:wrap;padding:1.25rem 0 1rem;border-bottom:1px solid var(--border);margin-bottom:1.5rem">
      <h1 style="font-size:1.15rem">Pay (UTXO)</h1>
      <div style="display:flex;align-items:center;gap:0.75rem;font-size:0.78rem;color:var(--text-muted)">
        <span class="nav-address">${escapeHtml(shortId(sub || ""))}</span>
        <button id="signout-btn" class="btn-link">Sign out</button>
      </div>
    </div>

    ${
    channel
      ? ""
      : `<div class="membership-banner revoked" style="position:static;margin-bottom:1.5rem">
          No channel configured. Open this page as
          <span class="mono">#/pay-utxo?channel=&lt;CHANNEL_CONTRACT_ID&gt;&amp;asset=&lt;ASSET_CONTRACT_ID&gt;</span>
          — deposits and sends need it.
        </div>`
  }

    <section class="empty-state" style="margin-bottom:1.5rem">
      <h2 style="margin:0 0 0.25rem;font-size:1rem">Balance</h2>
      <p style="color:var(--text-muted);font-size:0.82rem;margin-bottom:1rem">
        Funds this session controls inside the privacy channel.
      </p>
      <div class="stats-row" style="margin:0 0 1.25rem">
        <div class="stat-card active">
          <span class="stat-label">Session balance</span>
          <span class="stat-value" id="balance-value" style="font-variant-numeric:tabular-nums">0 XLM</span>
        </div>
        <div class="stat-card">
          <span class="stat-label">Unspent UTXOs</span>
          <span class="stat-value" id="utxo-count">0</span>
        </div>
        <div class="stat-card">
          <span class="stat-label">Channel</span>
          <span class="stat-value mono" style="font-size:0.8rem">${
    channel ? escapeHtml(shortId(channel.channelContractId)) : "—"
  }</span>
        </div>
      </div>
      <div class="form-row">
        <div class="form-group" style="margin-bottom:0">
          <label for="deposit-amount">Amount to deposit (XLM)</label>
          <input id="deposit-amount" placeholder="0.00" />
        </div>
        <button id="deposit-btn" class="btn-primary" ${
    channel ? "" : "disabled"
  }>Deposit</button>
      </div>
      <p class="hint-text">The wallet asks for one signature authorizing the
      transfer into the channel (fee ${fromStroops(DEPOSIT_FEE)} XLM). The
      deposit lands on a key generated for this session.</p>
      <div id="deposit-status"></div>
    </section>

    <section class="empty-state" style="margin-bottom:1.5rem">
      <h2 style="margin:0 0 0.25rem;font-size:1rem">Request transfer</h2>
      <p style="color:var(--text-muted);font-size:0.82rem;margin-bottom:1rem">
        Generate receiving keys and share them with whoever is paying you —
        over any channel you like.
      </p>
      <div class="form-row">
        <div class="form-group" style="margin-bottom:0">
          <label for="request-amount">Amount (XLM)</label>
          <input id="request-amount" placeholder="0.00" />
        </div>
        <div class="form-group" style="margin-bottom:0;max-width:140px">
          <label for="request-count">Keys</label>
          <input id="request-count" value="1" />
        </div>
        <button id="request-btn" class="btn-primary">Generate</button>
      </div>
      <p id="request-error" class="error-text" style="margin-top:0.75rem" hidden></p>
      <div id="request-output" hidden>
        <div class="form-group" style="margin-top:1rem;margin-bottom:0.5rem">
          <label>Share with the payer (CREATE operations)</label>
          <textarea id="request-blob" readonly rows="4" style="width:100%;padding:0.6rem 0.75rem;background:var(--bg);border:1px solid var(--border);border-radius:6px;color:var(--text);font-size:0.72rem;font-family:var(--font-mono);resize:vertical"></textarea>
        </div>
        <div style="display:flex;justify-content:space-between;align-items:center;gap:0.5rem;flex-wrap:wrap">
          <span class="hint-text" style="margin:0">Public — safe to share.</span>
          <button id="copy-blob-btn" class="btn-link">Copy</button>
        </div>
        <div style="display:flex;justify-content:space-between;align-items:center;gap:0.5rem;flex-wrap:wrap;margin-top:0.5rem">
          <span class="hint-text" style="margin:0">Secret keys — keep them;
          they leave with this session.</span>
          <button id="copy-secrets-btn" class="btn-link">Copy secrets</button>
        </div>
      </div>
    </section>

    <section class="empty-state">
      <h2 style="margin:0 0 0.25rem;font-size:1rem">Send</h2>
      <p style="color:var(--text-muted);font-size:0.82rem;margin-bottom:1rem">
        Paste the receiving keys someone shared with you. Sends spend this
        session's UTXOs — deposit first if the balance is short.
      </p>
      <div class="form-group">
        <label for="send-paste">Receiving keys from the recipient</label>
        <textarea id="send-paste" rows="4" placeholder="One CREATE operation per line" style="width:100%;padding:0.6rem 0.75rem;background:var(--bg);border:1px solid var(--border);border-radius:6px;color:var(--text);font-size:0.72rem;font-family:var(--font-mono);resize:vertical"></textarea>
      </div>
      <div id="send-summary" class="hint-text" style="margin:0 0 1rem"></div>
      <button id="send-btn" class="btn-primary btn-wide" ${
    channel ? "" : "disabled"
  }>Send</button>
      <p id="send-error" class="error-text" style="margin-top:0.75rem" hidden></p>
      <div id="send-status"></div>
    </section>
  `;

  const $ = <T extends HTMLElement>(sel: string) =>
    container.querySelector(sel) as T;

  // ── Balance / deposit ──
  const refreshBalance = () => {
    $("#balance-value").textContent = `${fromStroops(balance())} XLM` +
      (pendingAmount() > 0n
        ? ` (+${fromStroops(pendingAmount())} pending)`
        : "");
    $("#utxo-count").textContent = String(fundedUtxos().length);
  };
  refreshBalance();

  $("#signout-btn").addEventListener("click", () => {
    clearEntityAuth();
    clearEntityWallet();
    clearUtxoSession();
    container.replaceWith(payUtxoView());
  });

  $("#deposit-btn").addEventListener("click", async () => {
    if (!channel) return;
    const btn = $<HTMLButtonElement>("#deposit-btn");
    const statusEl = $("#deposit-status");
    btn.disabled = true;
    try {
      const amount = toStroops(
        $<HTMLInputElement>("#deposit-amount").value,
      );
      if (amount <= 0n) throw new Error("Enter a deposit amount");
      statusEl.innerHTML =
        `<p class="hint-text">Waiting for the wallet signature…</p>`;
      const { bundleId, utxo } = await submitDeposit(amount, channel);
      capture("entity_deposit_submitted", { bundleId });
      refreshBalance();
      const stop = pollBundle(bundleId, (state) => {
        statusEl.innerHTML = stepperHtml(state);
        if (state.status === "COMPLETED") {
          utxo.status = "FUNDED";
          refreshBalance();
          btn.disabled = false;
        } else if (state.status === "FAILED" || state.status === "EXPIRED") {
          utxo.status = "SPENT"; // never fund a failed deposit
          refreshBalance();
          btn.disabled = false;
        }
      }, (err) => {
        statusEl.innerHTML = stepperHtml(null, err);
        btn.disabled = false;
      });
      onCleanup(stop);
    } catch (e) {
      statusEl.innerHTML = stepperHtml(
        null,
        e instanceof Error ? e.message : String(e),
      );
      btn.disabled = false;
    }
  });

  // ── Request ──
  $("#request-btn").addEventListener("click", async () => {
    const errEl = $("#request-error");
    errEl.hidden = true;
    try {
      const amount = toStroops($<HTMLInputElement>("#request-amount").value);
      if (amount <= 0n) throw new Error("Enter the amount to request");
      const count = Number($<HTMLInputElement>("#request-count").value);
      const { mlxdrs, secretsHex } = await prepareReceive(amount, count);
      $<HTMLTextAreaElement>("#request-blob").value = mlxdrs.join("\n");
      $("#request-output").hidden = false;
      capture("entity_receive_generated", { count });

      $("#copy-blob-btn").onclick = () => {
        navigator.clipboard.writeText(mlxdrs.join("\n"));
        $("#copy-blob-btn").textContent = "Copied";
      };
      $("#copy-secrets-btn").onclick = () => {
        navigator.clipboard.writeText(secretsHex.join("\n"));
        $("#copy-secrets-btn").textContent = "Copied";
      };
    } catch (e) {
      errEl.textContent = e instanceof Error ? e.message : String(e);
      errEl.hidden = false;
    }
  });

  // ── Send ──
  const summaryEl = $("#send-summary");
  $("#send-paste").addEventListener("input", () => {
    const pasted = $<HTMLTextAreaElement>("#send-paste").value.trim();
    if (!pasted) {
      summaryEl.textContent = "";
      return;
    }
    try {
      const parsed = parseReceiverOps(pasted);
      const total = parsed.total + SEND_FEE;
      const have = balance();
      summaryEl.textContent = `Sending ${
        fromStroops(parsed.total)
      } XLM to ${parsed.ops.length} key(s) + ${
        fromStroops(SEND_FEE)
      } fee — session balance ${fromStroops(have)} XLM${
        have < total ? " (insufficient — deposit first)" : ""
      }`;
    } catch (e) {
      summaryEl.textContent = e instanceof Error ? e.message : String(e);
    }
  });

  $("#send-btn").addEventListener("click", async () => {
    if (!channel) return;
    const btn = $<HTMLButtonElement>("#send-btn");
    const errEl = $("#send-error");
    const statusEl = $("#send-status");
    errEl.hidden = true;
    btn.disabled = true;
    try {
      const parsed = parseReceiverOps(
        $<HTMLTextAreaElement>("#send-paste").value,
      );
      const { bundleId, change, spent } = await submitSend(parsed, channel);
      capture("entity_send_submitted", { bundleId });
      // Optimistically mark inputs spent so a second send can't reuse them.
      for (const u of spent) u.status = "SPENT";
      refreshBalance();
      const stop = pollBundle(bundleId, (state) => {
        statusEl.innerHTML = stepperHtml(state);
        if (state.status === "COMPLETED") {
          if (change) change.status = "FUNDED";
          refreshBalance();
          btn.disabled = false;
        } else if (state.status === "FAILED" || state.status === "EXPIRED") {
          // Spends didn't land — the inputs are still unspent on-chain.
          for (const u of spent) u.status = "FUNDED";
          if (change) change.status = "SPENT";
          refreshBalance();
          btn.disabled = false;
        }
      }, (err) => {
        statusEl.innerHTML = stepperHtml(null, err);
        btn.disabled = false;
      });
      onCleanup(stop);
    } catch (e) {
      errEl.textContent = e instanceof Error ? e.message : String(e);
      errEl.hidden = false;
      btn.disabled = false;
    }
  });

  return container;
}

// ── login flow (unchanged) ─────────────────────────────────────

export function payUtxoView(): HTMLElement {
  // Reset any entity state from a prior visit to this route in the same tab
  // — every visit starts at connect + SEP-10, mirroring the KYC route. The
  // authenticated surface is only ever reached via replaceWith after login.
  clearEntityAuth();
  clearEntityWallet();
  clearUtxoSession();

  const container = document.createElement("div");
  container.className = "login-container";

  container.innerHTML = `
    <div class="login-card">
      <h1>Pay (UTXO)</h1>

      <div id="step-connect">
        <p>Connect your Stellar wallet to send and receive through this provider.</p>
        <button id="connect-btn" class="btn-primary btn-wide">Connect Wallet</button>
      </div>

      <div id="step-signin" hidden>
        <p>Connected as:</p>
        <p class="mono" style="font-size:0.8rem;word-break:break-all;margin-bottom:1rem;color:var(--text-muted)"></p>
        <button id="signin-btn" class="btn-primary btn-wide">Sign In</button>
        <button id="change-wallet-btn" class="btn-link" style="margin-top:0.75rem;display:block;text-align:center;width:100%;color:var(--text-muted)">Use a different wallet</button>
      </div>

      <p id="pay-utxo-login-error" class="error-text" style="text-align:center" hidden></p>
    </div>
  `;

  const connectStep = container.querySelector(
    "#step-connect",
  ) as HTMLDivElement;
  const signinStep = container.querySelector("#step-signin") as HTMLDivElement;
  const errorEl = container.querySelector(
    "#pay-utxo-login-error",
  ) as HTMLParagraphElement;

  // Change wallet: clear the entity session and go back to step 1
  container.querySelector("#change-wallet-btn")?.addEventListener(
    "click",
    () => {
      clearEntityWallet();
      clearEntityAuth();
      connectStep.hidden = false;
      signinStep.hidden = true;
      errorEl.hidden = true;
      (container.querySelector("#connect-btn") as HTMLButtonElement).disabled =
        false;
    },
  );

  // Step 1: Connect Wallet
  container.querySelector("#connect-btn")?.addEventListener(
    "click",
    async () => {
      const btn = container.querySelector("#connect-btn") as HTMLButtonElement;
      btn.disabled = true;
      errorEl.hidden = true;

      try {
        const publicKey = await connectEntityWallet();

        connectStep.hidden = true;
        signinStep.hidden = false;
        const addrEl = signinStep.querySelector(".mono") as HTMLElement;
        addrEl.textContent = publicKey;

        capture("entity_wallet_connected", { publicKey });
      } catch (error) {
        errorEl.textContent = error instanceof Error
          ? error.message
          : "Failed to connect wallet";
        errorEl.hidden = false;
        btn.disabled = false;
      }
    },
  );

  // Step 2: Sign In (SEP-10 challenge co-sign)
  container.querySelector("#signin-btn")?.addEventListener(
    "click",
    async () => {
      const btn = container.querySelector("#signin-btn") as HTMLButtonElement;
      const originalText = btn.textContent;
      btn.disabled = true;
      errorEl.hidden = true;

      try {
        const { traceId } = startTrace();
        await withSpan("entity.login", traceId, async () => {
          btn.textContent = "Authenticating...";
          await authenticateEntity();
        });
        capture("entity_login", { publicKey: getEntityAddress() });

        container.replaceWith(paySurface());
      } catch (error) {
        errorEl.textContent = error instanceof Error
          ? error.message
          : String(error);
        errorEl.hidden = false;
        btn.textContent = originalText;
        btn.disabled = false;
      }
    },
  );

  return container;
}
