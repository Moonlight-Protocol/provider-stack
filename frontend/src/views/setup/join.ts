import { setupPage } from "./layout.ts";
import { escapeHtml } from "../../lib/dom.ts";
import { navigate } from "../../lib/router.ts";
import { capture } from "../../lib/analytics.ts";
import {
  type CouncilInfo,
  discoverCouncil,
  joinCouncil,
  signPayload,
} from "../../lib/api.ts";
import { derivePpKeypair } from "../../lib/wallet.ts";
import { clearAllDrafts, getFormDraft } from "../../lib/setup.ts";
import { API_BASE_URL } from "../../lib/config.ts";
import { startTrace, withSpan } from "../../lib/tracer.ts";

function renderStep(): HTMLElement {
  const el = document.createElement("div");
  const ppIndex = Number(sessionStorage.getItem("setup_pp_index") ?? "-1");
  const ppPublicKey = sessionStorage.getItem("setup_pp_publickey") || "";
  const meta = getFormDraft("metadata") as {
    name?: string;
    contactEmail?: string;
    jurisdictions?: string[];
  } | null;

  if (!ppPublicKey || ppIndex < 0) {
    navigate("/setup/metadata");
    return el;
  }

  el.innerHTML = `
    <h2>Council</h2>
    <p style="color:var(--text-muted);margin-bottom:1.5rem">
      Paste a council URL to discover and join it, or skip this step.
    </p>

    <div id="discover-section">
      <div class="form-group">
        <label for="council-url">Council URL</label>
        <input type="text" id="council-url" placeholder="https://council-platform.example.com" />
      </div>
      <button id="discover-btn" class="btn-primary">Discover</button>
      <p id="discover-error" class="error-text" style="margin-top:0.75rem" hidden></p>
    </div>

    <div id="council-info" hidden></div>

    <div id="join-section" hidden>
      <button id="join-btn" class="btn-primary btn-wide">Request to Join</button>
      <p id="join-error" class="error-text" style="margin-top:0.75rem" hidden></p>
    </div>

    <div style="margin-top:1.5rem">
      <button id="skip-btn" class="btn-primary btn-wide" style="background:var(--border)">Skip</button>
    </div>
  `;

  const urlInput = el.querySelector("#council-url") as HTMLInputElement;
  const discoverBtn = el.querySelector("#discover-btn") as HTMLButtonElement;
  const discoverError = el.querySelector(
    "#discover-error",
  ) as HTMLParagraphElement;
  const infoEl = el.querySelector("#council-info") as HTMLDivElement;
  const joinSection = el.querySelector("#join-section") as HTMLDivElement;
  const joinBtn = el.querySelector("#join-btn") as HTMLButtonElement;
  const joinError = el.querySelector("#join-error") as HTMLParagraphElement;

  let discovered: CouncilInfo | null = null;

  urlInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") discoverBtn.click();
  });

  discoverBtn.addEventListener("click", async () => {
    const url = urlInput.value.trim();
    if (!url) {
      discoverError.textContent = "Please enter a council URL";
      discoverError.hidden = false;
      return;
    }

    discoverBtn.disabled = true;
    discoverBtn.textContent = "Discovering...";
    discoverError.hidden = true;
    infoEl.hidden = true;
    joinSection.hidden = true;

    try {
      const { traceId } = startTrace();
      discovered = await withSpan(
        "council.discover",
        traceId,
        () => discoverCouncil(url),
        undefined,
        { "council.url": url },
      );

      const flags = discovered.jurisdictions.map((j) => {
        const flag = j.countryCode.toUpperCase().replace(
          /./g,
          (c) => String.fromCodePoint(0x1F1E6 + c.charCodeAt(0) - 65),
        );
        return `<span title="${
          escapeHtml(j.label || j.countryCode)
        }" style="font-size:1.2rem">${flag}</span>`;
      }).join(" ");
      const uniqueAssets = [
        ...new Set(discovered.channels.map((ch) => ch.assetCode)),
      ];
      const assets = uniqueAssets.map((code) =>
        `<span class="badge badge-active" style="margin-right:0.25rem">${
          escapeHtml(code)
        }</span>`
      ).join("");

      infoEl.innerHTML = `
        <div class="stat-card" style="margin:1rem 0;padding:1rem">
          <h3 style="margin:0 0 0.5rem;color:var(--text);font-size:1rem;text-transform:none;letter-spacing:0">${
        escapeHtml(discovered.council.name)
      }</h3>
          ${
        discovered.council.description
          ? `<p style="color:var(--text-muted);font-size:0.85rem;margin-bottom:0.5rem">${
            escapeHtml(discovered.council.description)
          }</p>`
          : ""
      }
          <div style="display:flex;gap:1.5rem;flex-wrap:wrap;font-size:0.85rem">
            <div><span class="stat-label">Jurisdictions</span><div style="margin-top:0.25rem">${
        flags || "--"
      }</div></div>
            <div><span class="stat-label">Assets</span><div style="margin-top:0.25rem">${
        assets || "--"
      }</div></div>
            <div><span class="stat-label">Providers</span><div style="margin-top:0.25rem">${discovered.providers.length}</div></div>
          </div>
        </div>
      `;
      infoEl.hidden = false;
      joinSection.hidden = false;
    } catch (err) {
      discoverError.textContent = err instanceof Error
        ? err.message
        : "Discovery failed";
      discoverError.hidden = false;
    } finally {
      discoverBtn.disabled = false;
      discoverBtn.textContent = "Discover";
    }
  });

  joinBtn.addEventListener("click", async () => {
    joinBtn.disabled = true;
    joinBtn.textContent = "Submitting...";
    joinError.hidden = true;

    try {
      const { traceId } = startTrace();
      await withSpan(
        "council.join_request",
        traceId,
        async () => {
          const derived = await derivePpKeypair(ppIndex);

          const joinPayload = {
            publicKey: ppPublicKey,
            councilId: discovered!.council.channelAuthId,
            label: typeof meta?.name === "string" ? meta.name : null,
            contactEmail: typeof meta?.contactEmail === "string"
              ? meta.contactEmail
              : null,
            jurisdictions: Array.isArray(meta?.jurisdictions) &&
                meta.jurisdictions.length > 0
              ? meta.jurisdictions
              : null,
            callbackEndpoint: new URL(API_BASE_URL).origin,
          };
          const signedEnvelope = await signPayload(
            joinPayload,
            derived.secretKey,
          );

          await joinCouncil({
            councilUrl: discovered!.councilUrl,
            councilId: discovered!.council.channelAuthId,
            councilName: discovered!.council.name,
            councilPublicKey: discovered!.council.councilPublicKey,
            ppPublicKey,
            signedEnvelope,
          });
        },
        undefined,
        {
          "council.url": discovered!.councilUrl,
          "council.id": discovered!.council.channelAuthId,
          "pp.public_key": ppPublicKey,
        },
      );

      capture("provider_join_request_submitted", {
        councilUrl: discovered!.councilUrl,
      });
      clearAllDrafts();
      navigate("/home");
    } catch (err) {
      joinError.textContent = err instanceof Error
        ? err.message
        : "Failed to submit";
      joinError.hidden = false;
      joinBtn.disabled = false;
      joinBtn.textContent = "Request to Join";
    }
  });

  el.querySelector("#skip-btn")?.addEventListener("click", () => {
    clearAllDrafts();
    navigate("/home");
  });

  return el;
}

export const joinView = setupPage("join", renderStep);
