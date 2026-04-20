import { useEffect, useState } from "react";
import { listDevices, type Device } from "../api/devices";

export default function Devices() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    listDevices()
      .then((d) => {
        if (!cancelled) setDevices(d);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <section className="space-y-4">
      <h2 className="text-2xl font-semibold">Devices</h2>
      {loading && <p className="text-white/60">Loading&hellip;</p>}
      {error && (
        <p className="text-amber-400 text-sm">
          Gateway unreachable ({error}). Bring up the stack with <code>just dev</code>.
        </p>
      )}
      {!loading && !error && devices.length === 0 && (
        <p className="text-white/60">No devices yet. Pair one to get started.</p>
      )}
      <ul className="space-y-2">
        {devices.map((d) => (
          <li
            key={d.id}
            className="rounded-lg bg-white/5 border border-white/10 p-3 flex items-baseline gap-3"
          >
            <span className="text-sm text-white/40 font-mono">{d.id.slice(0, 8)}</span>
            <span className="font-medium">{d.label || d.model || d.id}</span>
            <span className="ml-auto text-xs text-white/40">{d.integration}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}
