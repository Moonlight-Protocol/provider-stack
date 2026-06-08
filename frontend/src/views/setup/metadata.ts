import { setupPage } from "./layout.ts";
import { navigate } from "../../lib/router.ts";
import { COUNTRY_CODES } from "../../lib/jurisdictions.ts";
import { getFormDraft, saveFormDraft } from "../../lib/setup.ts";
import { escapeHtml } from "../../lib/dom.ts";
import { listPps } from "../../lib/api.ts";
import { derivePpKeypair } from "../../lib/wallet.ts";
import { accountExists } from "../../lib/horizon.ts";
import { startTrace, withSpan } from "../../lib/tracer.ts";

function renderStep(): HTMLElement {
  const el = document.createElement("div");

  const draft = getFormDraft("metadata") as {
    name?: string;
    contactEmail?: string;
    jurisdictions?: string[];
  } | null;

  el.innerHTML = `
    <h2>Provider</h2>
    <p style="color:var(--text-muted);margin-bottom:1.5rem">
      Tell us about your provider.
    </p>

    <div class="form-group">
      <label>Provider name *</label>
      <input type="text" id="pp-name" placeholder="Acme Privacy Inc" value="${
    escapeHtml(draft?.name ?? "")
  }" />
    </div>

    <div class="form-group">
      <label>Contact email</label>
      <input type="email" id="pp-email" placeholder="admin@acme.com" value="${
    escapeHtml(draft?.contactEmail ?? "")
  }" />
    </div>

    <div class="form-group">
      <label>Jurisdictions</label>
      <div id="jurisdiction-tags" class="jurisdiction-tags"></div>
      <input type="text" id="jurisdiction-filter" placeholder="Search countries..."
        style="width:100%;padding:0.6rem 0.75rem;background:var(--bg);border:1px solid var(--border);border-radius:6px;color:var(--text);font-size:0.875rem;font-family:var(--font-mono)" />
      <div class="jurisdiction-picker" id="jurisdiction-dropdown" hidden>
        <div id="jurisdiction-list" class="jurisdiction-list"></div>
      </div>
    </div>

    <p id="meta-error" class="error-text" hidden></p>

    <div style="margin-top:1.5rem">
      <button id="next-btn" class="btn-primary btn-wide">Next</button>
    </div>
  `;

  const selectedJurisdictions = new Set<string>(draft?.jurisdictions ?? []);
  const tagsEl = el.querySelector("#jurisdiction-tags") as HTMLDivElement;
  const filterEl = el.querySelector("#jurisdiction-filter") as HTMLInputElement;
  const listEl = el.querySelector("#jurisdiction-list") as HTMLDivElement;
  const errorEl = el.querySelector("#meta-error") as HTMLParagraphElement;

  function renderTags() {
    tagsEl.innerHTML = "";
    for (const code of selectedJurisdictions) {
      const entry = COUNTRY_CODES.find((c) => c.code === code);
      if (!entry) continue;
      const tag = document.createElement("span");
      tag.className = "jurisdiction-tag";
      tag.textContent = `${entry.code} `;
      const x = document.createElement("button");
      x.textContent = "\u00d7";
      x.style.cssText =
        "background:none;border:none;color:var(--text-muted);cursor:pointer;padding:0 0 0 0.25rem;font-size:1rem";
      x.addEventListener("click", () => {
        selectedJurisdictions.delete(code);
        renderTags();
        renderList(filterEl.value);
      });
      tag.appendChild(x);
      tagsEl.appendChild(tag);
    }
  }

  function renderList(filter: string) {
    listEl.innerHTML = "";
    const q = filter.toLowerCase();
    if (q.length < 2) {
      const hint = document.createElement("p");
      hint.style.cssText =
        "color:var(--text-muted);font-size:0.8rem;padding:0.5rem 0.75rem";
      hint.textContent = "Type at least 2 characters to search...";
      listEl.appendChild(hint);
      return;
    }
    for (const country of COUNTRY_CODES) {
      if (
        !country.label.toLowerCase().includes(q) &&
        !country.code.toLowerCase().includes(q)
      ) continue;
      const selected = selectedJurisdictions.has(country.code);
      const option = document.createElement("div");
      option.className = "jurisdiction-option" + (selected ? " selected" : "");
      const flag = country.code.toUpperCase().replace(
        /./g,
        (c: string) => String.fromCodePoint(0x1F1E6 + c.charCodeAt(0) - 65),
      );
      option.textContent = `${flag} ${country.label}`;
      option.addEventListener("click", () => {
        if (selected) selectedJurisdictions.delete(country.code);
        else selectedJurisdictions.add(country.code);
        renderTags();
        if (!selected) {
          filterEl.value = "";
          renderList("");
        } else renderList(filterEl.value);
      });
      listEl.appendChild(option);
    }
  }

  const dropdownEl = el.querySelector(
    "#jurisdiction-dropdown",
  ) as HTMLDivElement;

  filterEl.addEventListener("input", () => {
    renderList(filterEl.value);
    dropdownEl.hidden = false;
  });
  filterEl.addEventListener("focus", () => {
    dropdownEl.hidden = false;
  });
  // Delay hide so click events on options fire first
  filterEl.addEventListener("blur", () => {
    setTimeout(() => {
      dropdownEl.hidden = true;
    }, 200);
  });

  renderTags();

  el.querySelector("#next-btn")?.addEventListener("click", async () => {
    const name = (el.querySelector("#pp-name") as HTMLInputElement).value
      .trim();
    const contactEmail = (el.querySelector("#pp-email") as HTMLInputElement)
      .value.trim();
    const jurisdictions = Array.from(selectedJurisdictions);

    if (!name) {
      errorEl.textContent = "Provider name is required";
      errorEl.hidden = false;
      return;
    }

    const nextBtn = el.querySelector("#next-btn") as HTMLButtonElement;
    nextBtn.disabled = true;
    nextBtn.textContent = "Finding available slot...";
    errorEl.hidden = true;

    try {
      const { traceId } = startTrace();
      const index = await withSpan(
        "provider.find_pp_slot",
        traceId,
        async () => {
          // Scan for the first unused index
          const existingPps = await listPps();
          const existingKeys = new Set(existingPps.map((p) => p.publicKey));
          const MAX_SCAN = 20;

          for (let i = 0; i < MAX_SCAN; i++) {
            const kp = await derivePpKeypair(i);
            if (existingKeys.has(kp.publicKey)) continue;
            if (await accountExists(kp.publicKey)) continue;
            return i;
          }

          throw new Error("Could not find an available provider slot.");
        },
      );

      // Save draft and index for the fund step
      saveFormDraft("metadata", { name, contactEmail, jurisdictions });
      sessionStorage.setItem("setup_pp_index", String(index));

      navigate("/setup/fund");
    } catch (err) {
      errorEl.textContent = err instanceof Error ? err.message : "Failed";
      errorEl.hidden = false;
      nextBtn.disabled = false;
      nextBtn.textContent = "Next";
    }
  });

  return el;
}

export const metadataView = setupPage("metadata", renderStep);
