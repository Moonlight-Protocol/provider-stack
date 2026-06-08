/**
 * Minimal hash-based router for SPA navigation.
 *
 * Routes are defined as hash paths: #/login, #/home, #/provider/<pk>, etc.
 * Patterns may include `:name` segments which are extracted as params and
 * exposed to the handler via `getRouteParams()`.
 */

type RouteHandler = () => HTMLElement | Promise<HTMLElement>;

type RouteEntry = {
  segments: string[];
  paramNames: string[];
  handler: RouteHandler;
};

const routes: RouteEntry[] = [];
let cleanups: (() => void)[] = [];
let currentParams: Record<string, string> = {};
let currentQuery: URLSearchParams = new URLSearchParams();

function parsePattern(pattern: string): Omit<RouteEntry, "handler"> {
  const segments = pattern.split("/").filter((s) => s.length > 0);
  const paramNames: string[] = [];
  for (const seg of segments) {
    if (seg.startsWith(":")) paramNames.push(seg.slice(1));
  }
  return { segments, paramNames };
}

function matchPath(
  entry: RouteEntry,
  pathSegments: string[],
): Record<string, string> | null {
  if (entry.segments.length !== pathSegments.length) return null;
  const params: Record<string, string> = {};
  for (let i = 0; i < entry.segments.length; i++) {
    const patSeg = entry.segments[i];
    const pathSeg = pathSegments[i];
    if (patSeg.startsWith(":")) {
      params[patSeg.slice(1)] = decodeURIComponent(pathSeg);
    } else if (patSeg !== pathSeg) {
      return null;
    }
  }
  return params;
}

export function route(path: string, handler: RouteHandler): void {
  routes.push({ ...parsePattern(path), handler });
}

export function navigate(path: string): void {
  globalThis.location.hash = path;
}

export function getRouteParams(): Record<string, string> {
  return currentParams;
}

/**
 * Returns the parsed query string from the current hash route.
 * `#/foo/bar?x=1&y=2` → URLSearchParams of `x=1&y=2`.
 */
export function getRouteQuery(): URLSearchParams {
  return currentQuery;
}

async function render(): Promise<void> {
  const hash = globalThis.location.hash || "#/";
  const raw = hash.startsWith("#") ? hash.slice(1) : hash;
  const qIdx = raw.indexOf("?");
  const path = qIdx === -1 ? raw : raw.slice(0, qIdx);
  const queryString = qIdx === -1 ? "" : raw.slice(qIdx + 1);
  currentQuery = new URLSearchParams(queryString);
  const pathSegments = path.split("/").filter((s) => s.length > 0);

  let matched: { entry: RouteEntry; params: Record<string, string> } | null =
    null;
  for (const entry of routes) {
    const params = matchPath(entry, pathSegments);
    if (params) {
      matched = { entry, params };
      break;
    }
  }

  if (!matched) {
    const fallback = routes.find((r) =>
      r.segments.length === 1 && r.segments[0] === "404"
    );
    if (!fallback) return;
    matched = { entry: fallback, params: {} };
  }

  for (const fn of cleanups) {
    fn();
  }
  cleanups = [];

  const app = document.getElementById("app");
  if (!app) return;

  currentParams = matched.params;
  const element = await matched.entry.handler();
  app.innerHTML = "";
  app.appendChild(element);

  globalThis.scrollTo(0, 0);
}

export function startRouter(): void {
  globalThis.addEventListener("hashchange", render);
  render();
}

/**
 * Register a cleanup function for the current view.
 * All registered cleanups run before the next route renders.
 */
export function onCleanup(fn: () => void): void {
  cleanups.push(fn);
}
