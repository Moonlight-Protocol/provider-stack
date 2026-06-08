import { page } from "../components/page.ts";
import { escapeHtml } from "../lib/dom.ts";
import { navigate } from "../lib/router.ts";
import { listPps, registerPp } from "../lib/api.ts";
import { derivePpKeypair } from "../lib/wallet.ts";
import { accountExists } from "../lib/horizon.ts";

interface RecoveredPp {
  index: number;
  publicKey: string;
}

const MAX_CONSECUTIVE_MISSES = 5;
const MAX_SCAN = 50;

function renderContent(): HTMLElement {
  const el = document.createElement("div");
  el.style.maxWidth = "700px";
  el.style.margin = "0 auto";

  el.innerHTML = `
    <h2>Recover</h2>
    <p id="scan-status" style="color:var(--text-muted);margin-bottom:1.5rem">
      Scanning your wallet for existing providers...
    </p>
    <div id="scan-results"></div>
  `;

  const statusEl = el.querySelector("#scan-status") as HTMLParagraphElement;
  const resultsEl = el.querySelector("#scan-results") as HTMLDivElement;

  scanProviders(statusEl, resultsEl);
  return el;
}

async function scanProviders(
  statusEl: HTMLParagraphElement,
  resultsEl: HTMLDivElement,
) {
  const existingKeys = new Set<string>();
  try {
    const pps = await listPps();
    for (const pp of pps) existingKeys.add(pp.publicKey);
  } catch { /* platform may be empty */ }

  const entries: RecoveredPp[] = [];
  let consecutiveMisses = 0;
  let index = 0;

  while (consecutiveMisses < MAX_CONSECUTIVE_MISSES && index < MAX_SCAN) {
    statusEl.textContent = `Scanning index ${index}...`;

    const kp = await derivePpKeypair(index);

    if (existingKeys.has(kp.publicKey)) {
      consecutiveMisses = 0;
      index++;
      continue;
    }

    if (await accountExists(kp.publicKey)) {
      entries.push({ index, publicKey: kp.publicKey });
      consecutiveMisses = 0;
      index++;
      continue;
    }

    consecutiveMisses++;
    index++;
  }

  if (entries.length === 0) {
    statusEl.textContent = "No providers found for this wallet.";
    resultsEl.innerHTML = `
      <div class="empty-state" style="text-align:center;padding:2rem">
        <p style="color:var(--text-muted)">No existing providers were found on-chain. You can create a new one from the home page.</p>
        <button id="back-btn" class="btn-primary" style="margin-top:1rem">Back</button>
      </div>
    `;
    resultsEl.querySelector("#back-btn")?.addEventListener(
      "click",
      () => navigate("/home"),
    );
    return;
  }

  statusEl.textContent = "";

  const rows = entries.map((e) => `
    <tr>
      <td>${e.index + 1}</td>
      <td style="font-family:var(--font-mono);font-size:0.75rem;word-break:break-all">${
    escapeHtml(e.publicKey)
  }</td>
      <td style="text-align:right"><button class="icon-btn recover-btn" data-index="${e.index}" data-pk="${
    escapeHtml(e.publicKey)
  }" title="Recover"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 13v8l-4-4"/><path d="m12 21 4-4"/><path d="M4.393 15.269A7 7 0 1 1 15.71 8h1.79a4.5 4.5 0 0 1 2.436 8.284"/></svg></button></td>
    </tr>
  `).join("");

  resultsEl.innerHTML = `
    <table>
      <thead><tr><th>#</th><th>Address</th><th></th></tr></thead>
      <tbody>${rows}</tbody>
    </table>
  `;

  resultsEl.querySelectorAll(".recover-btn").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const idx = Number((btn as HTMLElement).dataset.index);
      (btn as HTMLButtonElement).disabled = true;

      try {
        const kp = await derivePpKeypair(idx);
        await registerPp(kp.secretKey, idx);
        const td = (btn as HTMLElement).closest("td")!;
        td.innerHTML = `<span class="badge badge-active">Registered</span>`;
      } catch (err) {
        console.error("Recovery failed:", err);
        (btn as HTMLButtonElement).disabled = false;
      }
    });
  });
}

export const recoverView = page(renderContent);
