import { setupPage } from "./layout.ts";
import { navigate } from "../../lib/router.ts";
import {
  derivePpKeypair,
  getConnectedAddress,
  signTransaction,
} from "../../lib/wallet.ts";
import { registerPp } from "../../lib/api.ts";
import {
  buildFundTx,
  getAccountBalance,
  submitHorizonTx,
} from "../../lib/stellar.ts";
import { getFormDraft } from "../../lib/setup.ts";

function renderStep(): HTMLElement {
  const el = document.createElement("div");
  const ppIndex = Number(sessionStorage.getItem("setup_pp_index") ?? "-1");
  const meta = getFormDraft("metadata") as {
    name?: string;
    contactEmail?: string;
    jurisdictions?: string[];
  } | null;

  if (ppIndex < 0 || !meta?.name) {
    navigate("/setup/metadata");
    return el;
  }

  el.innerHTML = `
    <h2>Treasury</h2>
    <p style="color:var(--text-muted);margin-bottom:1.5rem">
      Your provider needs XLM in its account to pay network fees.
    </p>

    <div class="stat-card" id="account-card" style="margin-bottom:1.5rem">
      <div style="display:flex;justify-content:space-between;align-items:center">
        <span class="stat-label">Provider Account</span>
        <div style="display:flex;gap:0.25rem">
          <button class="icon-btn" id="copy-btn" title="Copy address"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg></button>
          <button class="icon-btn" id="refresh-btn" title="Refresh balance"><svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 4 23 10 17 10"/><polyline points="1 20 1 14 7 14"/><path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15"/></svg></button>
        </div>
      </div>
      <span id="pp-address" class="mono" style="font-size:0.7rem;color:var(--text-muted);display:block;margin-top:0.5rem;word-break:break-all">Deriving...</span>
      <span id="balance-display" class="stat-value" style="font-size:1.25rem;display:block;margin-top:0.5rem">0.00 XLM</span>
    </div>

    <div class="stat-card" id="fund-card" style="margin-bottom:1.5rem">
      <span class="stat-label">Fund the account to operate</span>
      <div style="display:flex;gap:0.5rem;align-items:flex-end;margin-top:0.75rem">
        <div class="form-group" style="margin:0;flex:none;width:200px">
          <label for="fund-amount" style="white-space:nowrap">Amount (XLM)</label>
          <input type="number" id="fund-amount" value="10" min="1" step="1" />
        </div>
        <button id="fund-btn" class="btn-primary" style="padding:0.6rem 1.5rem">Fund Account</button>
      </div>
    </div>

    <p id="fund-error" class="error-text" hidden></p>

    <div style="margin-top:1.5rem">
      <button id="next-btn" class="btn-primary btn-wide" disabled>Next</button>
    </div>
  `;

  const accountCard = el.querySelector("#account-card") as HTMLDivElement;
  const fundCard = el.querySelector("#fund-card") as HTMLDivElement;
  const addressEl = el.querySelector("#pp-address") as HTMLElement;
  const balanceEl = el.querySelector("#balance-display") as HTMLElement;
  const nextBtn = el.querySelector("#next-btn") as HTMLButtonElement;
  const errorEl = el.querySelector("#fund-error") as HTMLParagraphElement;
  const fundBtn = el.querySelector("#fund-btn") as HTMLButtonElement;

  let ppPublicKey = "";
  let ppSecretKey = "";

  async function checkBalance() {
    if (!ppPublicKey) return;
    const { xlm, funded } = await getAccountBalance(ppPublicKey);
    const balance = funded ? parseFloat(xlm) : 0;

    balanceEl.textContent = `${balance.toFixed(2)} XLM`;

    if (balance > 0) {
      balanceEl.style.color = "var(--active)";
      accountCard.className = "stat-card active";
      fundCard.hidden = true;
      nextBtn.disabled = false;
    } else {
      balanceEl.style.color = "var(--text-muted)";
      accountCard.className = "stat-card";
      fundCard.hidden = false;
      nextBtn.disabled = true;
    }
  }

  // Derive PP address
  (async () => {
    const kp = await derivePpKeypair(ppIndex);
    ppPublicKey = kp.publicKey;
    ppSecretKey = kp.secretKey;
    addressEl.textContent = ppPublicKey;
    sessionStorage.setItem("setup_pp_publickey", ppPublicKey);
    await checkBalance();
  })();

  // Copy address
  el.querySelector("#copy-btn")?.addEventListener("click", () => {
    if (!ppPublicKey) return;
    navigator.clipboard.writeText(ppPublicKey).then(() => {
      const btn = el.querySelector("#copy-btn") as HTMLButtonElement;
      const orig = btn.innerHTML;
      btn.innerHTML =
        `<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--active)" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>`;
      setTimeout(() => {
        btn.innerHTML = orig;
      }, 1500);
    });
  });

  // Refresh balance
  el.querySelector("#refresh-btn")?.addEventListener(
    "click",
    () => checkBalance(),
  );

  // Fund via wallet payment
  fundBtn.addEventListener("click", async () => {
    const amountInput = el.querySelector("#fund-amount") as HTMLInputElement;
    const amount = amountInput.value.trim();

    if (!amount || parseFloat(amount) <= 0) {
      errorEl.textContent = "Enter a valid amount";
      errorEl.hidden = false;
      return;
    }

    fundBtn.disabled = true;
    fundBtn.textContent = "Building transaction...";
    errorEl.hidden = true;

    try {
      const sourceAddress = getConnectedAddress();
      if (!sourceAddress) throw new Error("Wallet not connected");

      const txXdr = await buildFundTx(sourceAddress, ppPublicKey, amount);
      fundBtn.textContent = "Sign in wallet...";
      const signedXdr = await signTransaction(txXdr);
      fundBtn.textContent = "Submitting...";
      await submitHorizonTx(signedXdr);

      fundBtn.textContent = "Funded!";
      await checkBalance();
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      errorEl.textContent = msg.includes("not found") || msg.includes("404")
        ? "Could not build transaction. Make sure your wallet has funds."
        : msg;
      errorEl.hidden = false;
      fundBtn.textContent = "Fund Account";
      fundBtn.disabled = false;
    }
  });

  // Register PP and continue
  nextBtn.addEventListener("click", async () => {
    nextBtn.disabled = true;
    nextBtn.textContent = "Registering provider...";
    errorEl.hidden = true;

    try {
      await registerPp(ppSecretKey, ppIndex, meta.name);

      // Store metadata locally
      const localMeta: Record<string, string | string[]> = {
        label: meta.name!,
      };
      if (meta.contactEmail) localMeta.contactEmail = meta.contactEmail;
      if (meta.jurisdictions && meta.jurisdictions.length > 0) {
        localMeta.jurisdictions = meta.jurisdictions;
      }
      localStorage.setItem(`pp_meta_${ppPublicKey}`, JSON.stringify(localMeta));

      navigate("/setup/join");
    } catch (err) {
      errorEl.textContent = err instanceof Error
        ? err.message
        : "Failed to register";
      errorEl.hidden = false;
      nextBtn.disabled = false;
      nextBtn.textContent = "Next";
    }
  });

  return el;
}

export const fundView = setupPage("fund", renderStep);
