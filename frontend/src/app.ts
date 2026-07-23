import { navigate, route, startRouter } from "./lib/router.ts";
import { initAnalytics } from "./lib/analytics.ts";
import { isAuthenticated } from "./lib/api.ts";
import { initTracer } from "./lib/tracer.ts";
import { OTEL_AUTH, OTEL_ENDPOINT } from "./lib/config.ts";

import { loginView } from "./views/login.ts";
import { providerView } from "./views/provider.ts";

// Public KYC/KYB submission — no auth, isolated from operator session
import { entitiesRegisterView } from "./views/entities/register.ts";

// Entity payment surfaces — SEP-10 entity session, module-local only,
// isolated from the operator session
import { payUtxoView } from "./views/pay-utxo.ts";
import { payNameView } from "./views/pay-name.ts";

// Initialize analytics (NOOP in dev)
initAnalytics();
initTracer({ endpoint: OTEL_ENDPOINT, auth: OTEL_AUTH });

// Single-PP stack: one operator, one provider. There is no provider list and
// no per-PP URL — the provider view IS the home view, mounted at "/".
route("/login", loginView);
route("/entities/register", entitiesRegisterView);
route("/pay-utxo", payUtxoView);
route("/pay-name", payNameView);

// Root — render the provider view directly when authed, otherwise login.
route("/", () => {
  if (isAuthenticated()) {
    return providerView();
  }
  navigate("/login");
  return document.createElement("div");
});

// 404
route("/404", () => {
  const el = document.createElement("div");
  el.className = "login-container";
  el.innerHTML =
    `<div class="login-card"><h1>404</h1><p>Page not found.</p><a href="#/">Back to home</a></div>`;
  return el;
});

// Start
startRouter();

// Dev-mode version check — __DEV_MODE__ is false in production, esbuild removes the block
import { checkVersions } from "./lib/version-check.ts";
declare const __DEV_MODE__: boolean;
if (__DEV_MODE__) {
  checkVersions().then((banner) => {
    if (banner) document.body.prepend(banner);
  });
}
