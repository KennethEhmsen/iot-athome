import { useEffect, useState, type ReactNode } from "react";
import type { User } from "oidc-client-ts";
import { AuthContext } from "./AuthContext";
import { OIDC_ENABLED, currentUser, userManager } from "./oidc";

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(null);
  const [loading, setLoading] = useState<boolean>(OIDC_ENABLED);

  useEffect(() => {
    if (!OIDC_ENABLED) {
      setLoading(false);
      return;
    }
    currentUser()
      .then(setUser)
      .finally(() => setLoading(false));

    const um = userManager;
    if (!um) return;
    const onLoaded = (u: User) => setUser(u);
    const onUnloaded = () => setUser(null);
    um.events.addUserLoaded(onLoaded);
    um.events.addUserUnloaded(onUnloaded);
    um.events.addAccessTokenExpired(onUnloaded);
    return () => {
      um.events.removeUserLoaded(onLoaded);
      um.events.removeUserUnloaded(onUnloaded);
      um.events.removeAccessTokenExpired(onUnloaded);
    };
  }, []);

  return (
    <AuthContext.Provider value={{ user, loading, enabled: OIDC_ENABLED }}>
      {children}
    </AuthContext.Provider>
  );
}
