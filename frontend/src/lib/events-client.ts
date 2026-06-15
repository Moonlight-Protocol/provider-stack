/**
 * WebSocket client for /api/v1/provider/events/ws on the single-PP stack.
 *
 * - URL derived from API_BASE_URL by swapping http(s):// → ws(s)://.
 * - Auth via `Sec-WebSocket-Protocol: moonlight.events.v1, bearer.<JWT>`.
 *   The server echoes only `moonlight.events.v1`; the JWT never lands in
 *   response headers / proxy logs.
 * - 30s ping/close handled by the server (Deno idleTimeout=30); the browser
 *   replies to native ping frames automatically.
 * - Exponential-backoff reconnect, capped at 30s.
 */
import { API_BASE_URL } from "./config.ts";

const TOKEN_KEY = "console_token";
const SUBPROTOCOL = "moonlight.events.v1";
const BACKOFF_BASE_MS = 1_000;
const BACKOFF_MAX_MS = 30_000;

// Mirrors src/core/service/events/event.types.ts in provider-platform.
export type EventScope = {
  ppPublicKey: string;
  ppLabel: string | null;
};

export type ProviderEvent =
  | {
    kind: "mempool.bundle_added";
    ts: number;
    scope: EventScope;
    payload: {
      bundleId: string;
      weight: number;
      channelContractId: string;
      newSlot: boolean;
      entityName: string | null;
      jurisdictions: string[];
      amount: string | null;
    };
  }
  | {
    kind: "mempool.bundle_expired";
    ts: number;
    scope: EventScope;
    payload: { bundleId: string; channelContractId: string };
  }
  | {
    kind: "executor.transaction_submitted";
    ts: number;
    scope: EventScope;
    payload: {
      txHash: string;
      bundleIds: string[];
      channelContractId: string;
    };
  }
  | {
    kind: "executor.execution_failed";
    ts: number;
    scope: EventScope;
    payload: {
      bundleIds: string[];
      channelContractId: string | null;
      reason: string;
    };
  }
  | {
    kind: "verifier.bundle_completed";
    ts: number;
    scope: EventScope;
    payload: {
      txId: string;
      bundleIds: string[];
      channelContractId: string;
    };
  }
  | {
    kind: "verifier.bundle_failed";
    ts: number;
    scope: EventScope;
    payload: {
      txId: string;
      bundleIds: string[];
      channelContractId: string;
      reason: string;
    };
  }
  | {
    kind: "channel.provider_added";
    ts: number;
    scope: EventScope;
    payload: { channelContractId: string };
  }
  | {
    kind: "channel.provider_removed";
    ts: number;
    scope: EventScope;
    payload: { channelContractId: string };
  }
  | {
    kind: "bundle.deposit_completed";
    ts: number;
    scope: EventScope;
    payload: {
      bundleId: string;
      txId: string;
      channelContractId: string;
      depositorAddress: string;
      amount: string;
    };
  }
  | {
    kind: "bundle.withdraw_completed";
    ts: number;
    scope: EventScope;
    payload: {
      bundleId: string;
      txId: string;
      channelContractId: string;
      recipientAddress: string;
      amount: string;
    };
  };

export type EventKind = ProviderEvent["kind"];
export type ConnectionStatus = "connecting" | "open" | "closed";
export type EventListener = (event: ProviderEvent) => void;
export type StatusListener = (status: ConnectionStatus) => void;

export interface EventsClientOptions {
  onEvent: EventListener;
  onStatus?: StatusListener;
}

export function wsUrlFromApiBase(base: string): string {
  return base.replace(/^http(s?):\/\//, (_match, s: string) => `ws${s}://`);
}

export class EventsClient {
  private socket: WebSocket | null = null;
  private retries = 0;
  private reconnectTimer: number | null = null;
  private closed = false;
  private opts: EventsClientOptions;

  constructor(opts: EventsClientOptions) {
    this.opts = opts;
  }

  start(): void {
    this.closed = false;
    this.connect();
  }

  stop(): void {
    this.closed = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket) {
      try {
        this.socket.close();
      } catch { /* ignore */ }
      this.socket = null;
    }
  }

  private connect(): void {
    const token = localStorage.getItem(TOKEN_KEY);
    if (!token) {
      this.opts.onStatus?.("closed");
      return;
    }
    const base = wsUrlFromApiBase(API_BASE_URL);
    const url = `${base}/provider/events/ws`;
    this.opts.onStatus?.("connecting");
    const sock = new WebSocket(url, [SUBPROTOCOL, `bearer.${token}`]);
    this.socket = sock;

    sock.addEventListener("open", () => {
      this.retries = 0;
      this.opts.onStatus?.("open");
    });
    sock.addEventListener("message", (evt) => {
      if (typeof evt.data !== "string") return;
      try {
        const parsed = JSON.parse(evt.data) as ProviderEvent;
        this.opts.onEvent(parsed);
      } catch {
        // swallow malformed payloads — server is the source of truth
      }
    });
    sock.addEventListener("close", () => {
      this.socket = null;
      this.opts.onStatus?.("closed");
      if (!this.closed) this.scheduleReconnect();
    });
    sock.addEventListener("error", () => {
      // The browser fires `error` then `close`; reconnect logic lives in close.
    });
  }

  private scheduleReconnect(): void {
    const delay = Math.min(
      BACKOFF_MAX_MS,
      BACKOFF_BASE_MS * 2 ** this.retries,
    );
    this.retries++;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      if (!this.closed) this.connect();
    }, delay) as unknown as number;
  }
}
