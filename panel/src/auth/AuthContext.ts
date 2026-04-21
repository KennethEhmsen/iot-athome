import { createContext } from "react";
import type { User } from "oidc-client-ts";

export interface AuthState {
  user: User | null;
  loading: boolean;
  enabled: boolean;
}

export const AuthContext = createContext<AuthState>({
  user: null,
  loading: true,
  enabled: false,
});
