import { Buffer } from "buffer";

// Node.js globals needed by wallets kit transitive deps (near-js, hot-wallet)
const g = globalThis as Record<string, unknown>;
g.__buffer_polyfill = { Buffer };
g.Buffer = Buffer;
if (!g.global) g.global = globalThis;
if (!g.process) g.process = { env: {}, version: "", browser: true };
