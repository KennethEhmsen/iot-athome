import { useEffect, useState } from "react";
import { useNavigate } from "react-router-dom";
import { completeSignIn } from "../auth/oidc";

/**
 * Keycloak redirects back here after a successful PKCE flow. We complete
 * the exchange, store the user, and bounce to the app root.
 */
export default function Callback() {
  const navigate = useNavigate();
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    completeSignIn()
      .then(() => navigate("/", { replace: true }))
      .catch((e: unknown) => setErr(e instanceof Error ? e.message : String(e)));
  }, [navigate]);

  return (
    <section className="space-y-3">
      <h2 className="text-2xl font-semibold">Signing in&hellip;</h2>
      {err && <p className="text-rose-400 text-sm">Sign-in failed: {err}</p>}
    </section>
  );
}
