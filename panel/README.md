# Panel

The IoT-AtHome web panel and Command Central kiosk surface.

## Stack

- Vite 6 + React 18 + TypeScript 5
- Tailwind CSS 3
- `react-router-dom` for routing
- `oidc-client-ts` for Keycloak OIDC (PKCE) — wired in W3
- `vite-plugin-pwa` for service worker + offline cache (design §6)
- Vitest + `@testing-library/react` for unit tests

## Dev

```bash
pnpm install
pnpm dev           # http://127.0.0.1:5173
pnpm test
pnpm build
pnpm lint
```

Vite proxies `/api/*` to `http://127.0.0.1:8081` (iot-gateway) and `/stream` to the matching WebSocket endpoint. Start the gateway with `just dev` and the Rust services.

## Kiosk shell

The Electron / WPE kiosk shell lands in M3.5. The PWA already supports install-to-homescreen on tablets through the service worker — not a proper kiosk lock-down, but usable as a "shared display" surface today.

## Auth

Dev Keycloak seeds three users (see `deploy/compose/keycloak/`):

| user | password | roles |
|---|---|---|
| `admin` | `DevPass!234` | iot-admin |
| `operator` | `DevPass!234` | iot-operator |
| `user` | `DevPass!234` | iot-user |

The OIDC flow isn't connected to the UI yet — W3.
