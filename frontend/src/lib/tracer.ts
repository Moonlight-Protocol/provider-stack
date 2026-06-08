/**
 * Lightweight browser-compatible OTEL tracer.
 * Exports spans via OTLP HTTP to Grafana Cloud (or any OTLP collector).
 * CSP-safe: no eval, no new Function.
 *
 * Same pattern as browser-wallet's telemetry.ts.
 */

interface SpanData {
  traceId: string;
  spanId: string;
  parentSpanId?: string;
  name: string;
  startTimeUnixNano: string;
  endTimeUnixNano?: string;
  attributes: Record<string, string | number | boolean>;
  status?: { code: number; message?: string };
}

const MAX_PENDING = 500;

let enabled = false;
let endpoint = "";
let authHeader = "";
const pendingSpans: SpanData[] = [];
let flushTimer: ReturnType<typeof setTimeout> | null = null;

// Module-level "current span" so outbound fetches can attach a traceparent
// header without every call site having to thread the span through. Safe for
// the sequential await chains in views; not safe for unrelated parallel work.
let currentSpan: SpanData | null = null;

function randomHex(bytes: number): string {
  const arr = new Uint8Array(bytes);
  crypto.getRandomValues(arr);
  return Array.from(arr, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function initTracer(config: { endpoint: string; auth?: string }): void {
  if (!config.endpoint) return;
  enabled = true;
  endpoint = config.endpoint;
  authHeader = config.auth ?? "";
  scheduleFlush();
}

export function startTrace(): { traceId: string; spanId: string } {
  return { traceId: randomHex(16), spanId: randomHex(8) };
}

export function startSpan(opts: {
  traceId: string;
  parentSpanId?: string;
  name: string;
  attributes?: Record<string, string | number | boolean>;
}): SpanData {
  return {
    traceId: opts.traceId,
    spanId: randomHex(8),
    parentSpanId: opts.parentSpanId,
    name: opts.name,
    startTimeUnixNano: String(Date.now() * 1_000_000),
    attributes: opts.attributes ?? {},
  };
}

export function endSpan(
  span: SpanData,
  status?: { code: number; message?: string },
): void {
  span.endTimeUnixNano = String(Date.now() * 1_000_000);
  if (status) span.status = status;
  if (enabled) {
    pendingSpans.push(span);
    if (pendingSpans.length > MAX_PENDING) pendingSpans.shift();
  }
}

/**
 * Trace an async operation. Creates a span, runs fn, ends the span.
 *
 * While fn is running, the span is exposed as the module-level current span
 * so outbound fetch calls can attach a W3C traceparent header.
 */
export async function withSpan<T>(
  name: string,
  traceId: string,
  fn: (span: SpanData) => Promise<T>,
  parentSpanId?: string,
  attributes?: Record<string, string | number | boolean>,
): Promise<T> {
  const span = startSpan({ traceId, parentSpanId, name, attributes });
  const previous = currentSpan;
  currentSpan = span;
  try {
    const result = await fn(span);
    endSpan(span, { code: 0 });
    return result;
  } catch (error) {
    endSpan(span, {
      code: 2,
      message: error instanceof Error ? error.message : String(error),
    });
    throw error;
  } finally {
    currentSpan = previous;
  }
}

/** W3C traceparent header value for chaining backend spans into this trace. */
export function traceparent(traceId: string, spanId: string): string {
  return `00-${traceId}-${spanId}-01`;
}

/** Returns the W3C traceparent header for the active span, or null. */
export function currentTraceparent(): string | null {
  if (!currentSpan) return null;
  return traceparent(currentSpan.traceId, currentSpan.spanId);
}

function scheduleFlush(): void {
  if (flushTimer) return;
  flushTimer = setTimeout(() => {
    flushTimer = null;
    flush();
  }, 5000);
}

async function flush(): Promise<void> {
  if (pendingSpans.length === 0) {
    if (enabled) scheduleFlush();
    return;
  }

  const spans = pendingSpans.splice(0, pendingSpans.length);

  const body = {
    resourceSpans: [{
      resource: {
        attributes: [
          { key: "service.name", value: { stringValue: "provider-console" } },
        ],
      },
      scopeSpans: [{
        scope: { name: "provider-console" },
        spans: spans.map((s) => ({
          traceId: s.traceId,
          spanId: s.spanId,
          parentSpanId: s.parentSpanId || "",
          name: s.name,
          kind: 3, // CLIENT
          startTimeUnixNano: s.startTimeUnixNano,
          endTimeUnixNano: s.endTimeUnixNano || s.startTimeUnixNano,
          attributes: Object.entries(s.attributes).map(([k, v]) => ({
            key: k,
            value: typeof v === "string"
              ? { stringValue: v }
              : typeof v === "number"
              ? { intValue: String(v) }
              : { boolValue: v },
          })),
          status: s.status
            ? { code: s.status.code, message: s.status.message || "" }
            : { code: 0 },
        })),
      }],
    }],
  };

  try {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
    };
    if (authHeader) headers["Authorization"] = authHeader;
    await fetch(`${endpoint}/v1/traces`, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
    });
  } catch (err) {
    console.warn("[tracer] Failed to export spans:", err);
  }

  if (enabled) scheduleFlush();
}
