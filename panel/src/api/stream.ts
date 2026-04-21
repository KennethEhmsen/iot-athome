/**
 * WebSocket client for `/stream`.
 *
 * The gateway decodes `iot.device.v1.EntityState` events into native JSON
 * (see `crates/iot-gateway/src/stream.rs`), so we don't need a protobuf
 * runtime in the browser.
 */

export type StreamStatus = "connecting" | "open" | "closed";

export interface StreamEvent {
  subject: string;
  iot_type: string;
  device_id?: string;
  entity_id?: string;
  value?: unknown;
  at?: string;
  payload_b64?: string;
  error?: string;
}

export interface StreamClient {
  close(): void;
  onEvent(fn: (e: StreamEvent) => void): void;
  onStatus(fn: (s: StreamStatus) => void): void;
}

/**
 * Open a reconnecting WebSocket to `/stream?topics=<filter>`. Reconnects
 * with linear back-off (capped at 10 s) on close or error.
 */
export function openStream(topics = "device.>"): StreamClient {
  const url =
    (location.protocol === "https:" ? "wss:" : "ws:") +
    "//" +
    location.host +
    "/stream?topics=" +
    encodeURIComponent(topics);

  const eventHandlers: Array<(e: StreamEvent) => void> = [];
  const statusHandlers: Array<(s: StreamStatus) => void> = [];
  let closed = false;
  let ws: WebSocket | null = null;
  let attempt = 0;

  const setStatus = (s: StreamStatus) => statusHandlers.forEach((fn) => fn(s));

  const connect = () => {
    if (closed) return;
    setStatus("connecting");
    ws = new WebSocket(url);
    ws.onopen = () => {
      attempt = 0;
      setStatus("open");
    };
    ws.onmessage = (msg) => {
      try {
        const parsed = JSON.parse(msg.data as string) as StreamEvent;
        eventHandlers.forEach((fn) => fn(parsed));
      } catch (e) {
        console.warn("stream parse error", e);
      }
    };
    const schedule = () => {
      if (closed) return;
      const delay = Math.min(10_000, 500 * ++attempt);
      setStatus("closed");
      setTimeout(connect, delay);
    };
    ws.onclose = schedule;
    ws.onerror = schedule;
  };

  connect();

  return {
    close() {
      closed = true;
      ws?.close();
    },
    onEvent(fn) {
      eventHandlers.push(fn);
    },
    onStatus(fn) {
      statusHandlers.push(fn);
    },
  };
}

/**
 * Parse `device.<plugin>.<device_id>.<entity>.state` into its components.
 * Returns `null` if the subject shape doesn't match.
 */
export function parseSubject(
  subject: string,
): { plugin: string; deviceId: string; entity: string; leaf: string } | null {
  const parts = subject.split(".");
  if (parts.length < 5 || parts[0] !== "device") return null;
  return {
    plugin: parts[1],
    deviceId: parts[2],
    entity: parts[3],
    leaf: parts.slice(4).join("."),
  };
}
