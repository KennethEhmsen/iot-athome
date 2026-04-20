import { NavLink, Route, Routes } from "react-router-dom";
import Devices from "./pages/Devices";
import Home from "./pages/Home";

export default function App() {
  return (
    <div className="min-h-screen flex flex-col">
      <header className="border-b border-white/10 px-6 py-4 flex items-center justify-between">
        <h1 className="text-xl font-semibold tracking-tight">IoT-AtHome</h1>
        <nav className="flex gap-4 text-sm">
          <NavLink to="/" className={linkClass} end>
            Home
          </NavLink>
          <NavLink to="/devices" className={linkClass}>
            Devices
          </NavLink>
        </nav>
        <Status />
      </header>
      <main className="flex-1 p-6 max-w-6xl w-full mx-auto">
        <Routes>
          <Route path="/" element={<Home />} />
          <Route path="/devices" element={<Devices />} />
        </Routes>
      </main>
      <footer className="border-t border-white/10 px-6 py-3 text-xs text-white/40">
        W1 skeleton &mdash; design locked, endpoints land in W3.
      </footer>
    </div>
  );
}

const linkClass = ({ isActive }: { isActive: boolean }) =>
  isActive ? "text-white" : "text-white/50 hover:text-white/80";

function Status() {
  // Placeholder for the live bus-health indicator that lands with the
  // WebSocket stream in W3.
  return (
    <div className="text-xs text-white/40" aria-label="connection status">
      offline
    </div>
  );
}
