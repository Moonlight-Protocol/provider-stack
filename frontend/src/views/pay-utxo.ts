/**
 * Entity UTXO payment surface at #/pay-utxo.
 *
 * Step 1 of the request-payment/transfer UI: entity SEP-10 login against the
 * existing GET/POST /stellar/auth endpoints. A separate name-based payment
 * surface lands later at #/pay-name.
 *
 * Isolation contract — same stance as the KYC route (entities/register.ts):
 *   - ZERO persistence: entity wallet address and entity JWT live in
 *     module-local state only; nothing survives refresh or re-entry. The
 *     entity reconnects and re-signs SEP-10 every visit.
 *   - No session bleed: never reads or writes the operator-auth storage.
 *   - No operator chrome — no nav, no operator logout.
 *
 * The Balance / Request Transfer / Send sections land in later steps.
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
import { capture } from "../lib/analytics.ts";
import { escapeHtml } from "../lib/dom.ts";
import { startTrace, withSpan } from "../lib/tracer.ts";

function authenticatedView(): HTMLElement {
  const sub = getEntityJwtSub();

  const container = document.createElement("div");
  container.className = "login-container";
  container.innerHTML = `
    <div class="login-card" style="max-width:520px">
      <h1>Pay (UTXO)</h1>
      <p>Signed in as:</p>
      <p class="mono" style="font-size:0.8rem;word-break:break-all;margin-bottom:1rem;color:var(--text-muted)">${
    escapeHtml(sub || "")
  }</p>
      <p style="color:var(--text-muted);font-size:0.85rem;margin-bottom:1.25rem">
        Balance, request transfer, and send will appear here.
      </p>
      <button id="signout-btn" class="btn-link" style="display:block;text-align:center;width:100%;color:var(--text-muted)">Sign out</button>
    </div>
  `;

  container.querySelector("#signout-btn")?.addEventListener("click", () => {
    clearEntityAuth();
    clearEntityWallet();
    container.replaceWith(payUtxoView());
  });

  return container;
}

export function payUtxoView(): HTMLElement {
  // Reset any entity state from a prior visit to this route in the same tab
  // — every visit starts at connect + SEP-10, mirroring the KYC route. The
  // authenticated card is only ever reached via replaceWith after login.
  clearEntityAuth();
  clearEntityWallet();

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

        container.replaceWith(authenticatedView());
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
