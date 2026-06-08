/**
 * Console configuration.
 * Reads from global config object set in index.html or defaults.
 */
declare global {
  interface Window {
    __CONSOLE_CONFIG__?: {
      apiBaseUrl?: string;
      stellarNetwork?: "testnet" | "mainnet" | "standalone";
      horizonUrl?: string;
      posthogKey?: string;
      posthogHost?: string;
      environment?: string;
      allowlist?: string[];
      otelEndpoint?: string;
      otelAuth?: string;
    };
  }
  // Mirror the Window-side declaration on globalThis so the typed access below
  // type-checks under `deno check` (the dom lib's globalThis doesn't extend Window
  // in Deno's type context).
  var __CONSOLE_CONFIG__: Window["__CONSOLE_CONFIG__"];
}

const config = globalThis.__CONSOLE_CONFIG__ ?? {};

export const API_BASE_URL = config.apiBaseUrl ?? "http://localhost:8000/api/v1";
export const STELLAR_NETWORK = config.stellarNetwork ?? "testnet";
// NOTE: The standalone fallback uses port 8000 which is the same as API_BASE_URL's
// default host. This is intentional — in standalone mode, the provider-platform
// reverse-proxies Horizon requests at the same origin. In production, horizonUrl
// should always be set explicitly via __CONSOLE_CONFIG__.
export const HORIZON_URL = config.horizonUrl ?? (
  STELLAR_NETWORK === "mainnet"
    ? "https://horizon.stellar.org"
    : STELLAR_NETWORK === "standalone"
    ? "http://localhost:8000"
    : "https://horizon-testnet.stellar.org"
);
export const POSTHOG_KEY = config.posthogKey ?? "";
export const POSTHOG_HOST = config.posthogHost ?? "https://us.i.posthog.com";
export const ENVIRONMENT = config.environment ?? "development";
export const IS_PRODUCTION = ENVIRONMENT === "production";
export const OTEL_ENDPOINT = config.otelEndpoint ?? "";
export const OTEL_AUTH = config.otelAuth ?? "";

export function isAllowed(address: string): boolean {
  const list = config.allowlist ?? [];
  return list.includes("*") || list.includes(address);
}
