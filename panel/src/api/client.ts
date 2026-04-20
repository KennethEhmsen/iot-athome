/**
 * Gateway HTTP client.
 *
 * The panel talks to the gateway via same-origin during dev (Vite proxy) and
 * directly to `https://<host>` in production (behind Envoy). The WS endpoint
 * for `/stream` is a separate concern — see `src/api/stream.ts` (W3).
 */

const BASE = "/api/v1";

export class GatewayError extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "GatewayError";
  }
}

export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    credentials: "include",
    headers: { "content-type": "application/json", accept: "application/json" },
    ...init,
  });
  if (!res.ok) {
    throw new GatewayError(res.status, `${res.status} ${res.statusText}`);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}
