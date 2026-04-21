/**
 * Gateway HTTP client.
 *
 * Same-origin during dev (Vite proxy), direct to `https://<host>` in prod
 * behind Envoy. When the panel has an OIDC access token, it's attached as
 * `Authorization: Bearer <jwt>`.
 */

import { getAccessToken, OIDC_ENABLED } from "../auth/oidc";

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
  const headers = new Headers({
    "content-type": "application/json",
    accept: "application/json",
    ...(init?.headers as HeadersInit | undefined),
  });
  if (OIDC_ENABLED) {
    const token = await getAccessToken();
    if (token) headers.set("authorization", `Bearer ${token}`);
  }
  const res = await fetch(`${BASE}${path}`, {
    credentials: "include",
    ...init,
    headers,
  });
  if (!res.ok) {
    throw new GatewayError(res.status, `${res.status} ${res.statusText}`);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return (await res.json()) as T;
}
