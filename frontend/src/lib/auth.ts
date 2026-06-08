import { clearSession } from "./wallet.ts";
import { clearPlatformAuth } from "./api.ts";
import { resetAnalytics } from "./analytics.ts";
import { navigate } from "./router.ts";

/**
 * Provider-console logout side effects. Centralised here so the nav's
 * onLogout callback (set up in the page wrapper) uses the same teardown
 * sequence.
 */
export function logout(): void {
  clearPlatformAuth();
  clearSession();
  resetAnalytics();
  navigate("/login");
}
