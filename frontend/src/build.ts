/**
 * Bundles src/app.ts into public/app.js for the browser.
 *
 * Uses esbuild via npm and the deno loader plugin for import-map resolution.
 * After bundling, applies post-build patches for Node built-ins that leak
 * through transitive deps:
 *   - `buffer`: CJS __require("buffer") patched to return globalThis polyfill,
 *     bare ESM imports removed (polyfill injected via src/shims/buffer.ts)
 *   - `node:crypto`: ESM import replaced with Web Crypto shim
 *
 * IMPORTANT — DO NOT REMOVE the `stellar-sdk` entry from deno.json's
 * imports. It looks unused (no `import` from src/) but it's load-bearing:
 * it pins the wallets-kit's `@stellar/stellar-sdk` peer dep. Without it,
 * Deno re-resolves with newer transitive deps that produce a cache directory
 * path exceeding macOS's 255-char filesystem limit, and the build fails with
 * "File name too long (os error 63)".
 */
import * as esbuild from "esbuild";
import { denoPlugins } from "@luca/esbuild-deno-loader";
import { fromFileUrl, resolve } from "@std/path";

const SRC_DIR = fromFileUrl(new URL(".", import.meta.url));
const PROJECT_ROOT = resolve(SRC_DIR, "..");
const ENTRY_POINT = resolve(SRC_DIR, "app.ts");
const BUFFER_SHIM = resolve(SRC_DIR, "shims/buffer.ts");
const OUTFILE = resolve(PROJECT_ROOT, "public/app.js");
const DENO_JSON = resolve(PROJECT_ROOT, "deno.json");
const HEALTH_OUT = resolve(PROJECT_ROOT, "public/health.json");

// Pinned @moonlight/ui tag. raw.githubusercontent.com serves CSS as
// text/plain with nosniff so browsers refuse @import of these URLs; we
// fetch + concatenate at build time and write the result to public/styles.css.
// Do not change without bumping the consumer-side deps explicitly.
const UI_LIB_TAG = "v0.3.1";
const UI_LIB_CSS_FILES = [
  "tokens/tokens.css",
  "base-styles/base-styles.css",
  "nav/nav.css",
  "stepper/stepper.css",
];
const APP_STYLES_SRC = resolve(SRC_DIR, "app-styles.css");
const STYLES_OUT = resolve(PROJECT_ROOT, "public/styles.css");

async function writeHealthJson(version: string): Promise<void> {
  const health = { status: "ok", service: "provider-console", version };
  await Deno.writeTextFile(HEALTH_OUT, JSON.stringify(health) + "\n");
  console.log(`Built public/health.json (provider-console ${version})`);
}

async function buildStyles(): Promise<void> {
  const parts: string[] = [];
  for (const path of UI_LIB_CSS_FILES) {
    const url =
      `https://raw.githubusercontent.com/Moonlight-Protocol/ui/${UI_LIB_TAG}/src/${path}`;
    const res = await fetch(url);
    if (!res.ok) {
      throw new Error(
        `Failed to fetch ${url}: ${res.status} ${res.statusText}`,
      );
    }
    const body = await res.text();
    parts.push(`/* @moonlight/ui ${UI_LIB_TAG} — ${path} */\n${body}`);
  }
  const appStyles = await Deno.readTextFile(APP_STYLES_SRC);
  parts.push(`/* provider-console app-styles */\n${appStyles}`);
  await Deno.writeTextFile(STYLES_OUT, parts.join("\n"));
  console.log(
    `Built public/styles.css from @moonlight/ui@${UI_LIB_TAG} + src/app-styles.css`,
  );
}

const isProduction = Deno.args.includes("--production");
const denoJson = JSON.parse(await Deno.readTextFile(DENO_JSON));
const version = denoJson.version ?? "0.0.0";

// public/ is a gitignored build artifact, so it won't exist on a fresh
// checkout (CI, Docker). Create it before writing the bundle/assets into it.
await Deno.mkdir(resolve(PROJECT_ROOT, "public"), { recursive: true });

await writeHealthJson(version);

await buildStyles();

await esbuild.build({
  entryPoints: [ENTRY_POINT],
  bundle: true,
  outfile: OUTFILE,
  format: "esm",
  platform: "browser",
  target: "es2022",
  supported: { decorators: false },
  minify: isProduction,
  sourcemap: true,
  define: {
    "__APP_VERSION__": JSON.stringify(version),
    "__DEV_MODE__": JSON.stringify(!isProduction),
  },
  inject: [BUFFER_SHIM],
  treeShaking: false,
  plugins: [
    // Deduplicate stellar XDR types. The bundle ends up with two copies
    // of js-xdr's Union class when both stellar-sdk's minified dist bundle
    // (which inlines stellar-base) AND the lib modules (which import
    // stellar-base separately) are included. XDR enum identity checks fail
    // across copies → "Bad union switch: [object Object]".
    //
    // Fix: intercept any resolve to stellar-sdk's dist bundles and redirect
    // to lib/index.js, ensuring only one copy of stellar-base is used.
    {
      name: "stellar-sdk-dedup",
      setup(build: esbuild.PluginBuild) {
        const sdkLib = resolve(
          PROJECT_ROOT,
          "node_modules/.deno/@stellar+stellar-sdk@15.0.1/node_modules/@stellar/stellar-sdk/lib/index.js",
        );
        build.onLoad(
          { filter: /stellar-sdk[/\\]dist[/\\]/ },
          () => {
            return {
              contents: `export * from ${JSON.stringify(sdkLib)};`,
              loader: "js",
            };
          },
        );
      },
    },
    // deno-lint-ignore no-explicit-any
    ...(denoPlugins({ configPath: DENO_JSON }) as any[]),
  ],
});

// ─── Post-build patches ────────────────────────────────────────
let appJs = await Deno.readTextFile(OUTFILE);

// 1. Patch __require: intercept require("buffer") before it throws.
// With nodeModulesDir, esbuild resolves CJS require("buffer") from
// node_modules/ directly — the "Dynamic require" error pattern may not
// exist. The patch is best-effort; skip if the pattern isn't found.
appJs = appJs.replace(
  /throw\s*(Error\('Dynamic require of "'\s*\+\s*(\w+)\s*\+\s*'" is not supported'\))/,
  (_match, errExpr, varName) =>
    `if(${varName}==="buffer")return globalThis.__buffer_polyfill;throw ${errExpr}`,
);

// 2. Remove bare ESM buffer imports
appJs = appJs.replace(
  /import\s*\{[^}]*\}\s*from\s*"(?:node:)?buffer"\s*;?/g,
  "",
);

// 3. Replace node:buffer imports with polyfill reference
appJs = appJs.replace(
  /import\s*\{([^}]*)\}\s*from\s*"node:buffer"\s*;?/g,
  (_match, names) => {
    const exports = names.split(",").map((n: string) => n.trim()).filter(
      Boolean,
    );
    return exports.map((n: string) => {
      const [original, alias] = n.split(/\s+as\s+/).map((s: string) =>
        s.trim()
      );
      const localName = alias || original;
      if (original === "Buffer") {
        return `var ${localName} = globalThis.__buffer_polyfill.Buffer;`;
      }
      return `var ${localName} = globalThis.__buffer_polyfill.${original};`;
    }).join("\n");
  },
);

// 4. Replace node:crypto import with Web Crypto shim
appJs = appJs.replace(
  /import\s*\{([^}]*)\}\s*from\s*"node:crypto"\s*;?/g,
  (_match, names) => {
    const exports = names.split(",").map((n: string) => n.trim()).filter(
      Boolean,
    );
    const shims: string[] = [];
    for (const name of exports) {
      if (name === "randomBytes") {
        shims.push(
          "var randomBytes = (size) => globalThis.crypto.getRandomValues(new Uint8Array(size));",
        );
      }
    }
    return shims.join("\n");
  },
);

// Defense in depth: any surviving `node:` specifier will be blocked by the
// browser CSP at runtime. Fail the build instead.
const surviving = appJs.match(/from\s*"node:[^"]+"/g);
if (surviving) {
  esbuild.stop();
  throw new Error(
    `Build failed: bundle contains node: specifiers that the browser cannot resolve:\n` +
      `  ${surviving.join("\n  ")}\n` +
      `Either extend the patches above, or remove the source-level import.`,
  );
}

await Deno.writeTextFile(OUTFILE, appJs);

esbuild.stop();
console.log(`Built public/app.js${isProduction ? " (production)" : ""}`);
