import { NavLink, Route, Routes } from "react-router-dom";
import { useAuth } from "./auth/AuthProvider";
import { signIn, signOut } from "./auth/oidc";
import { ErrorBoundary } from "./ErrorBoundary";
import Callback from "./pages/Callback";
import Devices from "./pages/Devices";
import Home from "./pages/Home";

export default function App() {
  return (
    <div className="min-h-screen flex flex-col">
      <header className="border-b border-white/10 px-6 py-4 flex items-center justify-between gap-4">
        <h1 className="text-xl font-semibold tracking-tight">IoT-AtHome</h1>
        <nav className="flex gap-4 text-sm">
          <NavLink to="/" className={linkClass} end>
            Home
          </NavLink>
          <NavLink to="/devices" className={linkClass}>
            Devices
          </NavLink>
        </nav>
        <UserBadge />
      </header>
      <main className="flex-1 p-6 max-w-6xl w-full mx-auto">
        <ErrorBoundary>
          <Routes>
            <Route path="/" element={<Home />} />
            <Route path="/devices" element={<Devices />} />
            <Route path="/callback" element={<Callback />} />
          </Routes>
        </ErrorBoundary>
      </main>
      <footer className="border-t border-white/10 px-6 py-3 text-xs text-white/40">
        M1 walking skeleton &mdash; plugin runtime + auth land in M2.
      </footer>
    </div>
  );
}

const linkClass = ({ isActive }: { isActive: boolean }) =>
  isActive ? "text-white" : "text-white/50 hover:text-white/80";

function UserBadge() {
  const { user, loading, enabled } = useAuth();
  if (!enabled) {
    return (
      <span className="text-xs text-white/40" title="OIDC disabled — dev mode">
        dev
      </span>
    );
  }
  if (loading) return <span className="text-xs text-white/40">…</span>;
  if (!user) {
    return (
      <button
        type="button"
        onClick={() => void signIn()}
        className="text-xs px-3 py-1 rounded bg-emerald-500/20 border border-emerald-500/40 text-emerald-200 hover:bg-emerald-500/30"
      >
        Sign in
      </button>
    );
  }
  const name = user.profile.preferred_username || user.profile.email || user.profile.sub;
  return (
    <div className="flex items-center gap-2 text-xs">
      <span className="text-white/70">{name}</span>
      <button
        type="button"
        onClick={() => void signOut()}
        className="text-white/40 hover:text-white/80"
      >
        Sign out
      </button>
    </div>
  );
}
