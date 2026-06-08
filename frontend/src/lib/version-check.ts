/**
 * Dev-mode version mismatch detection.
 * Compares local versions against latest GitHub releases.
 * Only runs in development; silently no-ops in production.
 */
import { API_BASE_URL } from "./config.ts";

declare const __APP_VERSION__: string;

interface VersionEntry {
  name: string;
  local: string;
  latest: string | null;
}

async function fetchLatestRelease(repo: string): Promise<string | null> {
  try {
    const res = await fetch(
      `https://api.github.com/repos/Moonlight-Protocol/${repo}/releases/latest`,
    );
    if (!res.ok) return null;
    const data = await res.json();
    return (data.tag_name ?? "").replace(/^v/, "");
  } catch {
    return null;
  }
}

async function fetchBackendHealth(): Promise<
  { version: string; deps: Record<string, string> } | null
> {
  try {
    const url = new URL(API_BASE_URL);
    url.pathname = "/api/v1/health";
    const res = await fetch(url.toString());
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function renderBanner(entries: VersionEntry[]): HTMLElement {
  const banner = document.createElement("div");
  banner.className = "version-mismatch-banner";

  const spans = entries.map((e) => {
    let color: string;
    let text: string;
    if (!e.latest) {
      color = "var(--pending)";
      text = `${esc(e.name)} v${esc(e.local)}`;
    } else if (e.local === e.latest) {
      color = "var(--active)";
      text = `${esc(e.name)} v${esc(e.local)}`;
    } else {
      color = "var(--inactive)";
      text = `${esc(e.name)} v${esc(e.local)} (latest: v${esc(e.latest)})`;
    }
    return `<span style="color:${color}">${text}</span>`;
  });

  banner.innerHTML = spans.join(
    ' <span style="color:var(--text-muted)">&middot;</span> ',
  );
  return banner;
}

export async function checkVersions(): Promise<HTMLElement | null> {
  try {
    const entries: VersionEntry[] = [];

    const appLatest = await fetchLatestRelease("provider-console");
    entries.push({
      name: "provider-console",
      local: __APP_VERSION__,
      latest: appLatest,
    });

    const health = await fetchBackendHealth();
    if (health) {
      const ppLatest = await fetchLatestRelease("provider-platform");
      entries.push({
        name: "provider-platform",
        local: health.version,
        latest: ppLatest,
      });

      const sdkVersion = health.deps?.["moonlight-sdk"];
      if (sdkVersion) {
        const sdkLatest = await fetchLatestRelease("moonlight-sdk");
        entries.push({
          name: "moonlight-sdk",
          local: sdkVersion,
          latest: sdkLatest,
        });
      }
    }

    if (entries.length === 0) return null;
    return renderBanner(entries);
  } catch {
    return null;
  }
}
