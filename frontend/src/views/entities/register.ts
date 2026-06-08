/**
 * Public KYC/KYB submission route at #/entities/register?provider=<ppPubkey>.
 *
 * Hard requirements — see prompt §4 Phase 3:
 *   - Public (no JWT). PP comes ONLY from ?provider=<ppPubkey>; missing →
 *     hard-error UI, no fallback.
 *   - ZERO persistence: no localStorage/sessionStorage/IndexedDB/cookies/
 *     window.name for any artifact (token, derived key, name input, signed
 *     challenge, wallet adapter state).
 *   - No session bleed: this route MUST NOT read the operator-auth storage
 *     and MUST NOT modify it. The wallet-kyc module enforces this.
 *   - No operator chrome — no nav, no logout, no "My providers" link.
 *
 * Flow: connect wallet → fetch challenge → sign nonce → POST entity.
 */
import { escapeHtml } from "../../lib/dom.ts";
import { getRouteQuery } from "../../lib/router.ts";
import { API_BASE_URL } from "../../lib/config.ts";
import {
  clearKycWallet,
  connectKycWallet,
  getKycAddress,
  signKycMessage,
} from "../../lib/wallet-kyc.ts";

const NAME_MAX_LEN = 250;

// Plain-text-only sanitisation: strip anything tag-shaped, collapse
// whitespace, trim. Server-side post.ts applies the same rule and is the
// authoritative gate; this is defence in depth.
function sanitiseNameClient(raw: string): string {
  return raw
    .replace(/<[^>]*>/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

function isValidPpPublicKey(value: string): boolean {
  // Stellar G-address: 56 chars, starts with G, base32 alphabet.
  return /^G[A-Z2-7]{55}$/.test(value);
}

function hardError(message: string): HTMLElement {
  const el = document.createElement("div");
  el.className = "login-container";
  el.innerHTML = `
    <div class="login-card">
      <h1>Cannot continue</h1>
      <p>${escapeHtml(message)}</p>
    </div>
  `;
  return el;
}

export function entitiesRegisterView(): HTMLElement {
  // Reset any wallet state from a prior visit to this route in the same tab.
  clearKycWallet();

  const query = getRouteQuery();
  const provider = query.get("provider") ?? "";

  if (!provider) {
    return hardError(
      "This page requires a ?provider=<PP_PUBLIC_KEY> query parameter naming the provider you are registering with.",
    );
  }
  if (!isValidPpPublicKey(provider)) {
    return hardError(
      "The ?provider value does not look like a valid Stellar public key.",
    );
  }

  const container = document.createElement("div");
  container.className = "login-container";
  container.innerHTML = `
    <div class="login-card" style="max-width:520px">
      <h1>Register entity</h1>
      <p style="color:var(--text-muted);font-size:0.85rem;margin-bottom:1.25rem">
        Submit identity information so this provider can process your bundles.
        Provider: <span class="mono" style="font-size:0.75rem;word-break:break-all">${
    escapeHtml(provider)
  }</span>
      </p>

      <div id="kyc-step-connect">
        <p>Connect your wallet to begin.</p>
        <button id="kyc-connect-btn" class="btn-primary btn-wide">Connect Wallet</button>
      </div>

      <div id="kyc-step-form" hidden>
        <p>Connected as:</p>
        <p class="mono" id="kyc-address" style="font-size:0.8rem;word-break:break-all;color:var(--text-muted);margin-bottom:1rem"></p>

        <div class="form-group">
          <label for="kyc-name">Legal name</label>
          <input type="text" id="kyc-name" maxlength="${NAME_MAX_LEN}" autocomplete="off" spellcheck="false" />
        </div>

        <button id="kyc-submit-btn" class="btn-primary btn-wide">Sign and submit</button>
      </div>

      <div id="kyc-step-success" hidden>
        <p style="color:var(--active);font-weight:600;margin-top:0.5rem">Submitted.</p>
        <p style="color:var(--text-muted);font-size:0.85rem">Your entity is registered with this provider. You can close this tab.</p>
      </div>

      <p id="kyc-error" class="error-text" style="text-align:center;margin-top:1rem" hidden></p>
    </div>
  `;

  const stepConnect = container.querySelector(
    "#kyc-step-connect",
  ) as HTMLDivElement;
  const stepForm = container.querySelector(
    "#kyc-step-form",
  ) as HTMLDivElement;
  const stepSuccess = container.querySelector(
    "#kyc-step-success",
  ) as HTMLDivElement;
  const connectBtn = container.querySelector(
    "#kyc-connect-btn",
  ) as HTMLButtonElement;
  const submitBtn = container.querySelector(
    "#kyc-submit-btn",
  ) as HTMLButtonElement;
  const addressEl = container.querySelector("#kyc-address") as HTMLElement;
  const nameInput = container.querySelector("#kyc-name") as HTMLInputElement;
  const errorEl = container.querySelector("#kyc-error") as HTMLParagraphElement;

  function showError(message: string): void {
    errorEl.textContent = message;
    errorEl.hidden = false;
  }
  function clearError(): void {
    errorEl.hidden = true;
    errorEl.textContent = "";
  }

  connectBtn.addEventListener("click", async () => {
    connectBtn.disabled = true;
    clearError();
    try {
      const address = await connectKycWallet();
      addressEl.textContent = address;
      stepConnect.hidden = true;
      stepForm.hidden = false;
      nameInput.focus();
    } catch (err) {
      showError(err instanceof Error ? err.message : "Failed to connect");
      connectBtn.disabled = false;
    }
  });

  submitBtn.addEventListener("click", async () => {
    clearError();
    const address = getKycAddress();
    if (!address) {
      showError("Wallet disconnected. Reload and reconnect.");
      return;
    }
    const name = sanitiseNameClient(nameInput.value);
    if (name.length === 0) {
      showError("Please enter your legal name.");
      return;
    }
    submitBtn.disabled = true;
    nameInput.disabled = true;

    try {
      const base = `${API_BASE_URL}/providers/${
        encodeURIComponent(provider)
      }/entities`;

      // 1. Fetch challenge.
      const challengeRes = await fetch(`${base}/challenge`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ pubkey: address }),
      });
      if (!challengeRes.ok) {
        const body = await challengeRes.json().catch(() => ({}));
        throw new Error(
          body.message || `Challenge failed (${challengeRes.status}).`,
        );
      }
      const { data: { nonce } } = await challengeRes.json();

      // 2. Sign the nonce.
      const signature = await signKycMessage(nonce);

      // 3. Submit entity.
      const submitRes = await fetch(base, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          pubkey: address,
          name,
          jurisdictions: [],
          signedChallenge: { nonce, signature },
        }),
      });
      if (!submitRes.ok && submitRes.status !== 409) {
        const body = await submitRes.json().catch(() => ({}));
        throw new Error(
          body.message || `Submit failed (${submitRes.status}).`,
        );
      }

      // Success: clear all in-memory state, show success card.
      clearKycWallet();
      nameInput.value = "";
      stepForm.hidden = true;
      stepSuccess.hidden = false;
    } catch (err) {
      showError(err instanceof Error ? err.message : "Submission failed");
      submitBtn.disabled = false;
      nameInput.disabled = false;
    }
  });

  return container;
}
