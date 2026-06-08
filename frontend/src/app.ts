import { navigate, route, startRouter } from "./lib/router.ts";
import { initAnalytics } from "./lib/analytics.ts";
import { isAuthenticated } from "./lib/api.ts";
import { isMasterSeedReady } from "./lib/wallet.ts";
import { initTracer } from "./lib/tracer.ts";
import { OTEL_AUTH, OTEL_ENDPOINT } from "./lib/config.ts";

import { loginView } from "./views/login.ts";
import { homeView } from "./views/home.ts";
import { providerView } from "./views/provider.ts";
import { recoverView } from "./views/recover.ts";

// Setup flow
import { metadataView } from "./views/setup/metadata.ts";
import { fundView } from "./views/setup/fund.ts";
import { joinView } from "./views/setup/join.ts";

// Public KYC/KYB submission — no auth, isolated from operator session
import { entitiesRegisterView } from "./views/entities/register.ts";

// Initialize analytics (NOOP in dev)
initAnalytics();
initTracer({ endpoint: OTEL_ENDPOINT, auth: OTEL_AUTH });

// Register routes
route("/login", loginView);
route("/home", homeView);
route("/provider/:pk", providerView);
route("/setup/metadata", metadataView);
route("/setup/fund", fundView);
route("/setup/join", joinView);
route("/recover", recoverView);
route("/entities/register", entitiesRegisterView);

// Root — redirect based on auth state
route("/", () => {
  if (isAuthenticated() && isMasterSeedReady()) {
    navigate("/home");
  } else {
    navigate("/login");
  }
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
