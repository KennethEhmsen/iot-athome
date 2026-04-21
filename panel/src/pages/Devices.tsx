import { useEffect, useState } from "react";
import { listDevices, type Device } from "../api/devices";
import { openStream, parseSubject, type StreamClient, type StreamStatus } from "../api/stream";
import { getAccessToken, OIDC_ENABLED } from "../auth/oidc";

interface EntityValue {
  value: unknown;
  at: string;
}

type LiveMap = Record<string, Record<string, EntityValue>>;

export default function Devices() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [status, setStatus] = useState<StreamStatus>("connecting");
  const [live, setLive] = useState<LiveMap>({});

  useEffect(() => {
    let alive = true;
    let client: StreamClient | undefined;

    const refresh = async () => {
      try {
        const d = await listDevices();
        if (alive) {
          setDevices(d);
          setError(null);
        }
      } catch (e) {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      }
    };

    refresh().finally(() => {
      if (alive) setLoading(false);
    });

    const onEvent = (evt: { subject: string; value?: unknown; at?: string }) => {
      if (!alive) return;
      const parsed = parseSubject(evt.subject);
      if (!parsed || parsed.leaf !== "state") return;
      if (parsed.entity.startsWith("_")) {
        void refresh();
        return;
      }
      if (evt.value === undefined) return;
      const deviceKey = parsed.deviceId.toLowerCase();
      setLive((prev) => ({
        ...prev,
        [deviceKey]: {
          ...(prev[deviceKey] ?? {}),
          [parsed.entity]: { value: evt.value, at: evt.at ?? "" },
        },
      }));
    };

    (async () => {
      const token = OIDC_ENABLED ? await getAccessToken() : null;
      if (!alive) return;
      client = openStream("device.>", token);
      client.onStatus((s) => {
        if (alive) setStatus(s);
      });
      client.onEvent(onEvent);
    })();

    return () => {
      alive = false;
      client?.close();
    };
  }, []);

  return (
    <section className="space-y-4">
      <div className="flex items-center justify-between">
        <h2 className="text-2xl font-semibold">Devices</h2>
        <StatusBadge status={status} />
      </div>
      {loading && <p className="text-white/60">Loading&hellip;</p>}
      {error && (
        <p className="text-amber-400 text-sm">
          Gateway unreachable ({error}). Bring up the stack with <code>just dev</code> and
          the registry + gateway binaries.
        </p>
      )}
      {!loading && !error && devices.length === 0 && (
        <p className="text-white/60">
          No devices yet. Pair one via a running adapter (e.g.{" "}
          <code>mosquitto_pub zigbee2mqtt/&lt;name&gt;</code>).
        </p>
      )}
      <ul className="space-y-2">
        {devices.map((d) => (
          <DeviceRow key={d.id} device={d} values={live[d.id.toLowerCase()]} />
        ))}
      </ul>
    </section>
  );
}

function DeviceRow({
  device,
  values,
}: {
  device: Device;
  values: Record<string, EntityValue> | undefined;
}) {
  const entries = values ? Object.entries(values).sort(([a], [b]) => a.localeCompare(b)) : [];
  return (
    <li className="rounded-lg bg-white/5 border border-white/10 p-3 space-y-2">
      <div className="flex items-baseline gap-3">
        <span className="text-xs text-white/40 font-mono">{device.id.slice(0, 8)}</span>
        <span className="font-medium">{device.label || device.model || device.id}</span>
        <span className="ml-auto text-xs text-white/40">{device.integration}</span>
      </div>
      {entries.length > 0 && (
        <ul className="flex flex-wrap gap-2 pt-1">
          {entries.map(([key, v]) => (
            <li
              key={key}
              className="text-xs bg-emerald-500/10 border border-emerald-500/30 rounded px-2 py-0.5"
              title={v.at}
            >
              <span className="text-emerald-400">{key}</span>
              <span className="text-white/70">: {String(v.value)}</span>
            </li>
          ))}
        </ul>
      )}
      {device.capabilities.length > 0 && entries.length === 0 && (
        <ul className="flex flex-wrap gap-1 pt-1">
          {device.capabilities.map((c) => (
            <li
              key={c}
              className="text-[10px] bg-white/5 border border-white/10 rounded px-1.5 py-0.5 text-white/50"
            >
              {c}
            </li>
          ))}
        </ul>
      )}
    </li>
  );
}

function StatusBadge({ status }: { status: StreamStatus }) {
  const classes: Record<StreamStatus, string> = {
    open: "bg-emerald-500/20 text-emerald-300 border-emerald-500/30",
    connecting: "bg-amber-500/20 text-amber-300 border-amber-500/30",
    closed: "bg-rose-500/20 text-rose-300 border-rose-500/30",
  };
  return (
    <span
      className={`text-xs border rounded px-2 py-0.5 font-mono ${classes[status]}`}
      aria-label={`stream ${status}`}
    >
      {status}
    </span>
  );
}
