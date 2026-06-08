/**
 * Setup wrapper — auth check, nav, stepper, and content slot.
 * Stepper rendering comes from @moonlight/ui; the step vocabulary
 * (SETUP_STEPS) stays app-side.
 */
import { renderNav } from "@moonlight/ui/nav";
import { pageLayout } from "@moonlight/ui/layout";
import { renderStepper } from "@moonlight/ui/stepper";
import { isAuthenticated } from "../../lib/api.ts";
import { getConnectedAddress, isMasterSeedReady } from "../../lib/wallet.ts";
import { isAllowed } from "../../lib/config.ts";
import { navigate } from "../../lib/router.ts";
import { logout } from "../../lib/auth.ts";
import { SETUP_STEPS, type SetupStepId } from "../../lib/setup.ts";

declare const __APP_VERSION__: string;

export function setupPage(
  currentStep: SetupStepId,
  renderStep: () => HTMLElement | Promise<HTMLElement>,
): () => Promise<HTMLElement> {
  return async () => {
    const addr = getConnectedAddress();
    if (
      !isAuthenticated() || !isMasterSeedReady() || (addr && !isAllowed(addr))
    ) {
      navigate("/login");
      return document.createElement("div");
    }

    const nav = renderNav({
      brand: "Provider Console",
      version: __APP_VERSION__,
      links: [{ href: "#/", label: "Home" }],
      address: addr,
      onLogout: logout,
    });

    const stepper = renderStepper({
      steps: SETUP_STEPS,
      currentStepId: currentStep,
    });

    const content = document.createElement("div");
    content.className = "onboarding-content";
    const rendered = await renderStep();
    content.appendChild(rendered);

    const main = document.createElement("div");
    main.appendChild(stepper);
    main.appendChild(content);

    return pageLayout(nav, main);
  };
}
