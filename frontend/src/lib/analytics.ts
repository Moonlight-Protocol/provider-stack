import { IS_PRODUCTION, POSTHOG_HOST, POSTHOG_KEY } from "./config.ts";

/**
 * PostHog analytics wrapper.
 * NOOP in development, real tracking in production.
 */

interface Analytics {
  capture(event: string, properties?: Record<string, unknown>): void;
  captureException(error: unknown, properties?: Record<string, unknown>): void;
  identify(distinctId: string, properties?: Record<string, unknown>): void;
  reset(): void;
}

const noop: Analytics = {
  capture() {},
  captureException() {},
  identify() {},
  reset() {},
};

let analytics: Analytics = noop;

export function initAnalytics(): void {
  if (!IS_PRODUCTION || !POSTHOG_KEY) {
    console.debug("[analytics] NOOP — not in production or no PostHog key");
    return;
  }

  // Dynamically load PostHog only in production
  const script = document.createElement("script");
  script.src = "https://us-assets.i.posthog.com/static/array.js";
  script.onload = () => {
    // deno-lint-ignore no-explicit-any
    const posthog = (window as any).posthog;
    if (posthog) {
      posthog.init(POSTHOG_KEY, {
        api_host: POSTHOG_HOST,
        person_profiles: "identified_only",
        capture_exceptions: true,
        loaded: () => {
          console.debug("[analytics] PostHog initialized");
        },
      });

      analytics = {
        capture: (event, properties) => posthog.capture(event, properties),
        captureException: (error, properties) =>
          posthog.captureException(error, properties),
        identify: (distinctId, properties) =>
          posthog.identify(distinctId, properties),
        reset: () => posthog.reset(),
      };
    }
  };
  document.head.appendChild(script);
}

export function capture(
  event: string,
  properties?: Record<string, unknown>,
): void {
  analytics.capture(event, properties);
}

export function captureException(
  error: unknown,
  properties?: Record<string, unknown>,
): void {
  analytics.captureException(error, properties);
}

export function identify(
  distinctId: string,
  properties?: Record<string, unknown>,
): void {
  analytics.identify(distinctId, properties);
}

export function resetAnalytics(): void {
  analytics.reset();
}
