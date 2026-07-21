/**
 * Entity UTXO payment surface at #/pay-utxo.
 *
 * Login: entity SEP-10 against GET/POST /stellar/auth (connect + sign),
 * then one master-seed signature ("Moonlight: Derive UTXO seed" — the
 * client-only seed that generates UTXO keys and recovers the balance by
 * sweeping them in index order) so keys derive deterministically from the
 * wallet.
 * A separate name-based surface lands later at #/pay-name.
 *
 * Signed in, three sections under the @moonlight/ui nav:
 *   Balance   — on-chain balance over the derived keys + deposit
 *   Request   — reserve derived keys, share the CREATE MLXDRs out of band
 *   Send      — paste receiver MLXDRs, spend funded derived UTXOs (no popup)
 *
 * Isolation contract — same stance as the KYC route (entities/register.ts):
 *   - ZERO persistence: address, JWT, seed, and keys live in module-local
 *     state only; nothing survives refresh. Everything re-derives from the
 *     wallet on the next visit.
 *   - No session bleed: never reads or writes the operator-auth storage.
 *
 * The channel is resolved from the provider itself (GET
 * /provider/entity/channels — the PP's council membership) and auto-selected
 * when there is exactly one. Channel-picker UX for multi-channel providers
 * is an open item.
 */
import { renderNav } from "@moonlight/ui/nav";
import { pageLayout } from "@moonlight/ui/layout";
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
  DEPOSIT_FEE,
  EntityNotApprovedError,
  fromStroops,
  getBundle,
  getEntityChannels,
  parseReceiverOps,
  prepareReceive,
  SEND_FEE,
  submitDeposit,
  submitSend,
  toStroops,
} from "../lib/moonlight-client.ts";
import {
  balance,
  clearDerivation,
  initEntitySeed,
  isSeedReady,
  refreshBalances,
} from "../lib/utxo-derivation.ts";
import { getNativeBalance } from "../lib/horizon.ts";
import { capture } from "../lib/analytics.ts";
import { escapeHtml } from "../lib/dom.ts";
import { navigate, onCleanup } from "../lib/router.ts";
import { startTrace, withSpan } from "../lib/tracer.ts";

declare const __APP_VERSION__: string;

// ── helpers ────────────────────────────────────────────────────

function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 4)}…${id.slice(-4)}` : id;
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

/**
 * Not-approved is actionable: link the entity straight to this provider's
 * KYC form instead of echoing a bare 403.
 */
function notApprovedHtml(providerPublicKey: string): string {
  const href = `#/entities/register?provider=${
    encodeURIComponent(providerPublicKey)
  }`;
  return `<p class="error-text" style="margin-top:1rem">
    This provider hasn't approved your account yet.
    <a href="${href}">Complete the registration form</a>, then come back
    and try again.</p>`;
}

function signOut(): void {
  clearEntityAuth();
  clearEntityWallet();
  clearDerivation();
  navigate("/pay-utxo");
  globalThis.dispatchEvent(new HashChangeEvent("hashchange"));
}

// ── authenticated surface ──────────────────────────────────────

async function paySurface(): Promise<HTMLElement> {
  const sub = getEntityJwtSub();
  const channels = await getEntityChannels().catch(() => []);
  const channel = channels[0] ?? null;

  const nav = renderNav({
    brand: "Pay (UTXO)",
    version: __APP_VERSION__,
    links: [],
    address: sub,
    onLogout: signOut,
  });

  const content = document.createElement("div");
  content.className = "container";
  content.style.maxWidth = "680px";
  content.innerHTML = `
    <section class="empty-state" style="margin:1.5rem 0">
      <div style="display:flex;justify-content:space-between;align-items:baseline">
        <h2 style="margin:0 0 0.25rem;font-size:1rem">Balance</h2>
        <button id="refresh-btn" class="btn-link" title="Refresh balances" aria-label="Refresh balances" style="font-size:1rem;line-height:1" ${
    channel ? "" : "disabled"
  }>⟳</button>
      </div>
      <div class="form-row" style="margin:0.5rem 0 1rem">
        <div class="form-group" style="margin-bottom:0;max-width:160px">
          <label for="deposit-asset">Asset</label>
          <select id="deposit-asset" style="width:100%;padding:0.6rem 0.75rem;background:var(--bg);border:1px solid var(--border);border-radius:6px;color:var(--text)"></select>
        </div>
        <div class="form-group" style="margin-bottom:0">
          <label for="deposit-amount">Amount</label>
          <input id="deposit-amount" placeholder="0.00" />
        </div>
        <button id="deposit-btn" class="btn-primary" disabled>Deposit</button>
      </div>
      <p id="balance-status" class="hint-text" style="margin:0 0 1rem"></p>
      <div id="asset-list"></div>
      <div id="deposit-status"></div>
    </section>

    <section class="empty-state" style="margin-bottom:1.5rem">
      <div style="display:flex;justify-content:space-between;align-items:baseline">
        <h2 style="margin:0 0 0.25rem;font-size:1rem">Request transfer</h2>
        <button id="request-advanced-btn" class="btn-link" title="Advanced options" aria-label="Advanced options" style="font-size:1rem;line-height:1">⚙</button>
      </div>
      <p style="color:var(--text-muted);font-size:0.82rem;margin-bottom:1rem">
        Generate a payment code to request a transfer for the chosen asset.
      </p>
      <div class="form-row">
        <div class="form-group" style="margin-bottom:0;max-width:160px">
          <label for="request-asset">Asset</label>
          <select id="request-asset" style="width:100%;padding:0.6rem 0.75rem;background:var(--bg);border:1px solid var(--border);border-radius:6px;color:var(--text)"></select>
        </div>
        <div class="form-group" style="margin-bottom:0">
          <label for="request-amount">Amount</label>
          <input id="request-amount" placeholder="0.00" />
        </div>
        <button id="request-copy-btn" class="btn-primary" disabled>Copy</button>
      </div>
      <div id="request-advanced" class="form-group" style="margin:0.75rem 0 0" hidden>
        <label for="request-count">Number of UTXOs</label>
        <input id="request-count" value="3" style="max-width:140px" />
      </div>
      <p id="request-error" class="error-text" style="margin-top:0.75rem" hidden></p>
    </section>

    <section class="empty-state" style="margin-bottom:3rem">
      <h2 style="margin:0 0 0.25rem;font-size:1rem">Send</h2>
      <p style="color:var(--text-muted);font-size:0.82rem;margin-bottom:1rem">
        Paste the receiving keys someone shared with you. Sends spend your
        funded UTXOs — deposit first if the balance is short.
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

  const wrapper = pageLayout(nav, content);
  const $ = <T extends HTMLElement>(sel: string) =>
    content.querySelector(sel) as T;

  // ── Balance: per-asset channel balances + wallet funds ──
  const depositAssetSelect = $<HTMLSelectElement>("#deposit-asset");
  channels.forEach((c, i) => {
    const opt = document.createElement("option");
    opt.value = String(i);
    opt.textContent = c.assetCode || c.label || shortId(c.assetContractId);
    depositAssetSelect.appendChild(opt);
  });

  // Wallet-side XLM balance (stroops) — deposits are gated on it.
  let walletBalance = 0n;
  const assetBalances = new Map<number, bigint>();

  const depositBtn = $<HTMLButtonElement>("#deposit-btn");
  const syncDepositBtn = () => {
    let ok = !!channel;
    if (ok) {
      try {
        const amount = toStroops($<HTMLInputElement>("#deposit-amount").value);
        ok = amount > 0n && amount + DEPOSIT_FEE <= walletBalance;
      } catch {
        ok = false;
      }
    }
    depositBtn.disabled = !ok;
  };
  $("#deposit-amount").addEventListener("input", syncDepositBtn);
  $("#deposit-asset").addEventListener("input", syncDepositBtn);

  const refreshUI = () => {
    $("#asset-list").innerHTML = channels.map((c, i) => {
      const code = escapeHtml(
        c.assetCode || c.label || shortId(c.assetContractId),
      );
      const bal = assetBalances.get(i);
      return `<div style="display:flex;justify-content:space-between;align-items:center;padding:0.55rem 0;border-top:1px solid var(--border)">
        <span>${code}</span>
        <span style="font-variant-numeric:tabular-nums">${
        bal === undefined ? "—" : escapeHtml(fromStroops(bal))
      }</span>
      </div>`;
    }).join("");
  };
  refreshUI();

  const loadBalances = async () => {
    if (!channel) {
      $("#balance-status").textContent = "This provider has no active channel.";
      return;
    }
    const statusEl = $("#balance-status");
    try {
      // The seed derives at sign-in; this surface never raises a wallet
      // popup of its own accord.
      if (!isSeedReady()) {
        statusEl.textContent = "No derived keys — sign out and back in.";
        return;
      }
      statusEl.textContent = "Checking your balances…";
      const address = getEntityAddress();
      if (address) walletBalance = await getNativeBalance(address);
      // Refresh every asset's channel; the deposit-selected one last, so
      // reserve/spend state is left pointing at it.
      const selected = Number(depositAssetSelect.value) || 0;
      const order = channels
        .map((c, i) => ({ c, i }))
        .sort((a, b) => (a.i === selected ? 1 : 0) - (b.i === selected ? 1 : 0));
      for (const { c, i } of order) {
        await refreshBalances(c);
        assetBalances.set(i, balance());
      }
      statusEl.textContent = "";
      refreshUI();
      syncDepositBtn();
    } catch (e) {
      statusEl.textContent = "";
      const msg = e instanceof Error ? e.message : String(e);
      $("#deposit-status").innerHTML = stepperHtml(null, msg);
    }
  };
  loadBalances();

  $("#refresh-btn").addEventListener("click", () => loadBalances());

  // ── Deposit ──
  depositBtn.addEventListener("click", async () => {
    const depositChannel = channels[Number(depositAssetSelect.value) || 0];
    if (!depositChannel) return;
    const btn = depositBtn;
    const statusEl = $("#deposit-status");
    btn.disabled = true;
    try {
      const amount = toStroops($<HTMLInputElement>("#deposit-amount").value);
      if (amount <= 0n) throw new Error("Enter a deposit amount");
      statusEl.innerHTML =
        `<p class="hint-text">Waiting for the wallet signature…</p>`;
      const bundleId = await submitDeposit(amount, depositChannel);
      capture("entity_deposit_submitted", { bundleId });
      const stop = pollBundle(bundleId, (state) => {
        statusEl.innerHTML = stepperHtml(state);
        if (
          state.status === "COMPLETED" || state.status === "FAILED" ||
          state.status === "EXPIRED"
        ) {
          syncDepositBtn();
          if (state.status === "COMPLETED") loadBalances();
        }
      }, (err) => {
        statusEl.innerHTML = stepperHtml(null, err);
        syncDepositBtn();
      });
      onCleanup(stop);
    } catch (e) {
      statusEl.innerHTML = e instanceof EntityNotApprovedError
        ? notApprovedHtml(e.providerPublicKey)
        : stepperHtml(null, e instanceof Error ? e.message : String(e));
      syncDepositBtn();
    }
  });

  // ── Request ──
  const assetSelect = $<HTMLSelectElement>("#request-asset");
  channels.forEach((c, i) => {
    const opt = document.createElement("option");
    opt.value = String(i);
    opt.textContent = c.assetCode || c.label || shortId(c.assetContractId);
    assetSelect.appendChild(opt);
  });

  $("#request-advanced-btn").addEventListener("click", () => {
    const adv = $("#request-advanced");
    adv.hidden = !adv.hidden;
  });

  const copyBtn = $<HTMLButtonElement>("#request-copy-btn");
  const requestInputsValid = (): boolean => {
    try {
      const amount = toStroops($<HTMLInputElement>("#request-amount").value);
      const count = Number($<HTMLInputElement>("#request-count").value);
      return amount > 0n && Number.isInteger(count) && count >= 1;
    } catch {
      return false;
    }
  };
  const syncCopyBtn = () => {
    copyBtn.disabled = !requestInputsValid();
  };
  for (const sel of ["#request-amount", "#request-count", "#request-asset"]) {
    $(sel).addEventListener("input", syncCopyBtn);
  }

  // One payment code per filled-in form: re-clicking with unchanged inputs
  // copies the same code instead of reserving another set of keys.
  let lastRequest = { key: "", code: "" };
  copyBtn.addEventListener("click", async () => {
    const errEl = $("#request-error");
    errEl.hidden = true;
    copyBtn.disabled = true;
    try {
      const amount = toStroops($<HTMLInputElement>("#request-amount").value);
      const count = Number($<HTMLInputElement>("#request-count").value);
      const key = `${assetSelect.value}|${amount}|${count}`;
      if (lastRequest.key !== key) {
        const mlxdrs = await prepareReceive(amount, count);
        lastRequest = { key, code: mlxdrs.join("\n") };
        capture("entity_receive_generated", { count });
      }
      await navigator.clipboard.writeText(lastRequest.code);
      copyBtn.textContent = "Copied";
      setTimeout(() => {
        copyBtn.textContent = "Copy";
        syncCopyBtn();
      }, 3000);
    } catch (e) {
      errEl.textContent = e instanceof Error ? e.message : String(e);
      errEl.hidden = false;
      syncCopyBtn();
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
      } fee — balance ${fromStroops(have)} XLM${
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
      if (!isSeedReady()) await loadBalances();
      const parsed = parseReceiverOps(
        $<HTMLTextAreaElement>("#send-paste").value,
      );
      const { bundleId } = await submitSend(parsed, channel);
      capture("entity_send_submitted", { bundleId });
      const stop = pollBundle(bundleId, (state) => {
        statusEl.innerHTML = stepperHtml(state);
        if (
          state.status === "COMPLETED" || state.status === "FAILED" ||
          state.status === "EXPIRED"
        ) {
          btn.disabled = false;
          loadBalances();
        }
      }, (err) => {
        statusEl.innerHTML = stepperHtml(null, err);
        btn.disabled = false;
      });
      onCleanup(stop);
    } catch (e) {
      if (e instanceof EntityNotApprovedError) {
        statusEl.innerHTML = notApprovedHtml(e.providerPublicKey);
      } else {
        errEl.textContent = e instanceof Error ? e.message : String(e);
        errEl.hidden = false;
      }
      btn.disabled = false;
    }
  });

  return wrapper;
}

// ── login flow ─────────────────────────────────────────────────

export function payUtxoView(): HTMLElement {
  // Reset any entity state from a prior visit to this route in the same tab
  // — every visit starts at connect + SEP-10, mirroring the KYC route. The
  // authenticated surface is only ever reached via replaceWith after login.
  clearEntityAuth();
  clearEntityWallet();
  clearDerivation();

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
      clearDerivation();
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
          // Seed derivation is part of the login ceremony: both signatures
          // belong to the one user action ("Sign In"), never to a popup the
          // page raises on its own later — same as the other apps' login →
          // initMasterSeed chain. Freighter rejects back-to-back popups
          // without a pause.
          btn.textContent = "Deriving keys...";
          await new Promise((r) => setTimeout(r, 1000));
          await initEntitySeed();
        });
        capture("entity_login", { publicKey: getEntityAddress() });

        container.replaceWith(await paySurface());
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
