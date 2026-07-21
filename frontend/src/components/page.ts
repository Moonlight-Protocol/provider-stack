import { renderNav } from "@moonlight/ui/nav";
import { pageLayout } from "@moonlight/ui/layout";
import { checkMembershipStatus, getPp, isAuthenticated } from "../lib/api.ts";
import { getConnectedAddress } from "../lib/wallet.ts";
import { isAllowed } from "../lib/config.ts";
import { navigate } from "../lib/router.ts";
import { logout } from "../lib/auth.ts";

declare const __APP_VERSION__: string;

const BANNER_ID = "membership-banner";

/**
 * Background membership check for the single PP.
 * Detects revocations (ACTIVE → not ACTIVE) and shows a banner.
 * If a revocation banner is already showing but the PP is active again, removes it.
 */
async function checkMemberships(wrapper: HTMLElement) {
  let pp;
  try {
    pp = await getPp();
  } catch {
    return;
  }

  const activeMembership = pp.councilMemberships.find(
    (m) => m.status === "ACTIVE",
  );
  if (!activeMembership) return;

  try {
    const result = await checkMembershipStatus();
    if (result !== "ACTIVE") {
      showBanner(
        wrapper,
        `Your provider "${pp.label || pp.publicKey}" was removed from ${
          activeMembership.councilName || "the council"
        }.`,
      );
      return;
    }
  } catch { /* silently fail */ }

  // No revocation — drop any stale banner from a previous tick.
  document.getElementById(BANNER_ID)?.remove();
}

function showBanner(wrapper: HTMLElement, message: string) {
  document.getElementById(BANNER_ID)?.remove();

  const banner = document.createElement("div");
  banner.id = BANNER_ID;
  banner.className = "membership-banner revoked";
  banner.textContent = message;

  // Insert after nav, before main
  const nav = wrapper.querySelector("nav");
  if (nav?.nextSibling) {
    wrapper.insertBefore(banner, nav.nextSibling);
  } else {
    wrapper.prepend(banner);
  }
}

/**
 * Wraps a view with the nav bar and auth check.
 */
export function page(
  renderContent: () => HTMLElement | Promise<HTMLElement>,
): () => Promise<HTMLElement> {
  return async () => {
    const addr = getConnectedAddress();
    if (
      !isAuthenticated() || (addr && !isAllowed(addr))
    ) {
      navigate("/login");
      return document.createElement("div");
    }

    const nav = renderNav({
      brand: "Provider Console",
      version: __APP_VERSION__,
      links: [
        { href: "#/", label: "Home" },
      ],
      address: addr,
      onLogout: logout,
    });
    const content = await renderContent();
    const wrapper = pageLayout(nav, content);

    // Intentionally fire-and-forget — membership checks are best-effort and
    // should not block page rendering or cancel on navigation.
    checkMemberships(wrapper);

    return wrapper;
  };
}
