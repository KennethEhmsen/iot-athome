import { UserManager, WebStorageStateStore, type User } from "oidc-client-ts";

/**
 * OIDC configuration. Driven by Vite env:
 *   VITE_OIDC_AUTHORITY   e.g. http://localhost:8080/realms/iotathome
 *   VITE_OIDC_CLIENT_ID   e.g. iot-panel
 *
 * When `VITE_OIDC_AUTHORITY` is absent the panel assumes the gateway is
 * running in dev-mode (no auth) and short-circuits the login flow.
 */

const AUTHORITY = import.meta.env.VITE_OIDC_AUTHORITY as string | undefined;
const CLIENT_ID = (import.meta.env.VITE_OIDC_CLIENT_ID as string | undefined) ?? "iot-panel";

export const OIDC_ENABLED = !!AUTHORITY;

export const userManager: UserManager | null = AUTHORITY
  ? new UserManager({
      authority: AUTHORITY,
      client_id: CLIENT_ID,
      redirect_uri: `${location.origin}/callback`,
      silent_redirect_uri: `${location.origin}/silent-renew.html`,
      post_logout_redirect_uri: location.origin,
      response_type: "code",
      scope: "openid profile email",
      loadUserInfo: true,
      automaticSilentRenew: true,
      userStore: new WebStorageStateStore({ store: window.localStorage }),
    })
  : null;

export async function currentUser(): Promise<User | null> {
  if (!userManager) return null;
  return userManager.getUser();
}

export async function signIn(): Promise<void> {
  if (!userManager) return;
  await userManager.signinRedirect();
}

export async function completeSignIn(): Promise<User | null> {
  if (!userManager) return null;
  return userManager.signinRedirectCallback();
}

export async function signOut(): Promise<void> {
  if (!userManager) return;
  await userManager.signoutRedirect();
}

export async function getAccessToken(): Promise<string | null> {
  const u = await currentUser();
  return u?.access_token ?? null;
}
