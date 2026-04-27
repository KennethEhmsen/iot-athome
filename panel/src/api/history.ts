/**
 * Gateway `/devices/{id}/history` client (M5b W1).
 *
 * Returns rows already decoded from `iot.device.v1.EntityState` —
 * the panel never sees raw protobuf. `value` is whatever JSON shape
 * the entity carries (number, string, bool, struct). The chart on
 * the DeviceHistory page only plots numeric values; non-numeric
 * rows render as a text trace below.
 */

import { GatewayError, request } from "./client";

export interface HistoryRow {
  /** Device ULID (matches the `Device.id` from the registry). */
  device_id: string;
  /** Full bus subject the row was captured from, e.g.
   * `device.zigbee2mqtt.<id>.temperature.state`. */
  subject: string;
  /** RFC 3339 server-side capture timestamp (UTC). */
  at: string;
  /** Entity ULID extracted from the protobuf, or `undefined` when
   * the row's payload didn't decode as `EntityState`. */
  entity_id?: string;
  /** Decoded `EntityState.value` — number, string, bool, or struct.
   * `undefined` when decode failed. */
  value?: unknown;
  /** Original message bytes, base64-STD encoded. The panel doesn't
   * use this; surfaced for tooling that wants raw access. */
  payload_b64: string;
}

interface HistoryResponse {
  rows: HistoryRow[];
}

export interface HistoryRange {
  /** Inclusive start. RFC 3339 string. */
  from?: Date;
  /** Inclusive end. RFC 3339 string. */
  to?: Date;
  /** Hard limit on rows. Gateway clamps to 5000. Default 500. */
  limit?: number;
}

/**
 * Fetch the historical state rows for a device within `range`.
 *
 * Returns rows newest-first per the gateway contract — the chart
 * sorts ascending for plotting.
 *
 * Throws `GatewayError` with `status === 503` when the host wasn't
 * started with `IOT_TIMESCALE_URL` (history is opt-in). The page
 * surfaces this as a clear "history disabled" message rather than a
 * generic fetch error.
 */
export async function fetchHistory(
  deviceId: string,
  range: HistoryRange = {},
): Promise<HistoryRow[]> {
  const params = new URLSearchParams();
  if (range.from) params.set("from", range.from.toISOString());
  if (range.to) params.set("to", range.to.toISOString());
  if (range.limit !== undefined) params.set("limit", String(range.limit));
  const qs = params.toString();
  const path = `/devices/${encodeURIComponent(deviceId)}/history${qs ? `?${qs}` : ""}`;
  const r = await request<HistoryResponse>(path);
  return r.rows;
}

/** Re-export the GatewayError class so the page can `instanceof`-check
 * for the 503 history-disabled response without importing two
 * modules. */
export { GatewayError };
