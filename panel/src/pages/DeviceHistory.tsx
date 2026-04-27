import { useEffect, useMemo, useState } from "react";
import { Link, useParams } from "react-router-dom";
import {
  CartesianGrid,
  Legend,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import { getDevice, type Device } from "../api/devices";
import { fetchHistory, GatewayError, type HistoryRow } from "../api/history";

/**
 * DeviceHistory page (M5b W1).
 *
 * Plots numeric `EntityState.value` over time, one line per
 * `entity_id`, against the configurable time range. Non-numeric
 * rows (booleans, strings, structs) render below the chart as a
 * compact text log so the page is still useful for switches /
 * scenes / discrete events.
 *
 * Data path:
 *   gateway GET /devices/{id}/history → JSON rows with decoded
 *   `value` + `entity_id` (no protobuf runtime in the browser).
 *
 * The chart is `recharts` (~40 KB gzipped, declarative, no
 * imperative D3 wiring). Time ranges are preset chips for the
 * common cases — power-user URL params (?from=&to=) drop in
 * trivially when M5b W2 needs them.
 */

type RangeKey = "1h" | "24h" | "7d" | "30d";

const RANGE_PRESETS: Record<RangeKey, { label: string; ms: number }> = {
  "1h": { label: "Last hour", ms: 60 * 60 * 1000 },
  "24h": { label: "Last 24 hours", ms: 24 * 60 * 60 * 1000 },
  "7d": { label: "Last 7 days", ms: 7 * 24 * 60 * 60 * 1000 },
  "30d": { label: "Last 30 days", ms: 30 * 24 * 60 * 60 * 1000 },
};

/** A point on the chart. Recharts wants flat objects; we pivot the
 * `entity_id`-keyed rows into one column per entity. */
interface ChartPoint {
  /** Unix-ms — used as the X axis numeric value. */
  t: number;
  /** Entity → numeric value at this timestamp. Sparse: rows that
   * didn't carry data for an entity at this t leave it `undefined`,
   * which recharts renders as a gap (correct — we don't have data,
   * we shouldn't fake interpolation). */
  [entity: string]: number | undefined;
}

/** Tailwind-friendly palette. Cycled per entity in stable index
 * order so re-renders don't re-shuffle colours. */
const LINE_COLORS = [
  "#34d399", // emerald-400
  "#60a5fa", // blue-400
  "#fbbf24", // amber-400
  "#f87171", // red-400
  "#a78bfa", // violet-400
  "#22d3ee", // cyan-400
  "#fb923c", // orange-400
  "#a3e635", // lime-400
];

export default function DeviceHistory() {
  const { id } = useParams<{ id: string }>();
  const [device, setDevice] = useState<Device | null>(null);
  const [rows, setRows] = useState<HistoryRow[]>([]);
  const [rangeKey, setRangeKey] = useState<RangeKey>("24h");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!id) return;
    let alive = true;
    setLoading(true);
    setError(null);

    const range = RANGE_PRESETS[rangeKey];
    const from = new Date(Date.now() - range.ms);

    Promise.all([
      getDevice(id).catch((e: unknown) => {
        // Device 404 is recoverable — chart still renders if rows
        // come back, just without the friendly title.
        console.warn("getDevice failed", e);
        return null;
      }),
      fetchHistory(id, { from, limit: 5000 }),
    ])
      .then(([dev, hr]) => {
        if (!alive) return;
        setDevice(dev);
        setRows(hr);
      })
      .catch((e: unknown) => {
        if (!alive) return;
        if (e instanceof GatewayError && e.status === 503) {
          setError(
            "History backend is disabled. Start the host with IOT_TIMESCALE_URL to enable timescale-backed history.",
          );
        } else {
          setError(e instanceof Error ? e.message : String(e));
        }
      })
      .finally(() => {
        if (alive) setLoading(false);
      });

    return () => {
      alive = false;
    };
  }, [id, rangeKey]);

  const { entities, points, nonNumeric } = useMemo(() => groupRows(rows), [rows]);

  return (
    <section className="space-y-4">
      <header className="flex items-baseline gap-3 flex-wrap">
        <Link to="/devices" className="text-sm text-white/50 hover:text-white/80">
          ← Devices
        </Link>
        <h2 className="text-2xl font-semibold">{device?.label || device?.model || id}</h2>
        <span className="text-xs text-white/40 font-mono">{id}</span>
      </header>

      <RangePicker selected={rangeKey} onSelect={setRangeKey} />

      {loading && <p className="text-white/60">Loading history&hellip;</p>}
      {error && <p className="text-amber-400 text-sm">{error}</p>}

      {!loading && !error && (
        <>
          <ChartCard entities={entities} points={points} />
          <NonNumericLog rows={nonNumeric} />
          <p className="text-xs text-white/40">
            {rows.length.toLocaleString()} rows · {entities.length}{" "}
            {entities.length === 1 ? "entity" : "entities"}
          </p>
        </>
      )}
    </section>
  );
}

function RangePicker({
  selected,
  onSelect,
}: {
  selected: RangeKey;
  onSelect: (k: RangeKey) => void;
}) {
  return (
    <div className="flex gap-2 flex-wrap">
      {(Object.keys(RANGE_PRESETS) as RangeKey[]).map((k) => {
        const active = k === selected;
        return (
          <button
            key={k}
            type="button"
            onClick={() => onSelect(k)}
            className={
              active
                ? "text-xs px-3 py-1 rounded bg-emerald-500/30 border border-emerald-500/50 text-emerald-100"
                : "text-xs px-3 py-1 rounded bg-white/5 border border-white/10 text-white/60 hover:text-white/90"
            }
            aria-pressed={active}
          >
            {RANGE_PRESETS[k].label}
          </button>
        );
      })}
    </div>
  );
}

function ChartCard({ entities, points }: { entities: string[]; points: ChartPoint[] }) {
  if (entities.length === 0 || points.length === 0) {
    return (
      <div className="rounded-lg bg-white/5 border border-white/10 p-6 text-sm text-white/60">
        No numeric history in the selected range. Toggle to a wider window or check the non-numeric
        log below if this device only emits discrete events.
      </div>
    );
  }
  return (
    <div className="rounded-lg bg-white/5 border border-white/10 p-3">
      <ResponsiveContainer width="100%" height={320}>
        <LineChart data={points} margin={{ top: 8, right: 12, bottom: 0, left: 0 }}>
          <CartesianGrid stroke="#ffffff15" strokeDasharray="3 3" />
          <XAxis
            dataKey="t"
            type="number"
            domain={["dataMin", "dataMax"]}
            tickFormatter={fmtTickTime}
            stroke="#ffffff60"
            fontSize={11}
          />
          <YAxis stroke="#ffffff60" fontSize={11} />
          <Tooltip
            contentStyle={{
              background: "#0a0a0a",
              border: "1px solid #ffffff20",
              fontSize: 12,
            }}
            labelFormatter={(v) => fmtTickTime(v as number)}
          />
          <Legend wrapperStyle={{ fontSize: 12 }} />
          {entities.map((e, i) => (
            <Line
              key={e}
              type="monotone"
              dataKey={e}
              stroke={LINE_COLORS[i % LINE_COLORS.length]}
              dot={false}
              isAnimationActive={false}
              connectNulls={false}
              strokeWidth={1.5}
            />
          ))}
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}

function NonNumericLog({ rows }: { rows: HistoryRow[] }) {
  if (rows.length === 0) return null;
  // Show newest first, cap at 50 to keep DOM small.
  const head = rows.slice(0, 50);
  return (
    <div className="rounded-lg bg-white/5 border border-white/10 p-3 space-y-1">
      <h3 className="text-sm font-semibold text-white/80">Discrete / non-numeric</h3>
      <ul className="space-y-0.5 font-mono text-xs">
        {head.map((r, i) => (
          <li key={`${r.at}-${i}`} className="flex gap-3 text-white/70">
            <span className="text-white/40 shrink-0">{fmtRowTime(r.at)}</span>
            <span className="text-emerald-300 shrink-0">{entityFromSubject(r.subject)}</span>
            <span className="text-white/80 truncate">{stringifyValue(r.value)}</span>
          </li>
        ))}
      </ul>
      {rows.length > head.length && (
        <p className="text-xs text-white/40 pt-1">
          &hellip; {rows.length - head.length} earlier rows hidden.
        </p>
      )}
    </div>
  );
}

// ---------------------------------------------------------- helpers

interface GroupedRows {
  entities: string[];
  points: ChartPoint[];
  nonNumeric: HistoryRow[];
}

/**
 * Pivot the gateway's `[{at, entity_id, value}, ...]` row stream into
 * recharts' wide format (`[{t, <entity>: number, ...}, ...]`),
 * separating non-numeric rows for the discrete log below the chart.
 *
 * Rows arrive newest-first from the gateway; we sort ascending here
 * so the chart's X axis time direction matches the eye's left-to-
 * right reading order.
 *
 * Two rows at the same millisecond on the same entity merge —
 * later wins. That's near-impossible at TimescaleDB µs precision
 * but a defensive accumulator beats a flaky chart.
 */
function groupRows(rows: HistoryRow[]): GroupedRows {
  const ascending = [...rows].sort((a, b) => a.at.localeCompare(b.at));
  const entities = new Set<string>();
  const nonNumeric: HistoryRow[] = [];
  const byT = new Map<number, ChartPoint>();

  for (const r of ascending) {
    if (typeof r.value !== "number" || !Number.isFinite(r.value)) {
      nonNumeric.push(r);
      continue;
    }
    const ent = r.entity_id || entityFromSubject(r.subject) || "value";
    entities.add(ent);
    const t = Date.parse(r.at);
    if (Number.isNaN(t)) continue; // malformed ts — drop, don't poison the chart
    const existing = byT.get(t);
    if (existing) {
      existing[ent] = r.value;
    } else {
      byT.set(t, { t, [ent]: r.value });
    }
  }

  // Newest-first for the discrete log so the latest event is at the
  // top of the visible window.
  nonNumeric.sort((a, b) => b.at.localeCompare(a.at));

  return {
    entities: Array.from(entities).sort(),
    points: Array.from(byT.values()).sort((a, b) => a.t - b.t),
    nonNumeric,
  };
}

/** Pull `<entity>` out of `device.<plugin>.<device_id>.<entity>.state`.
 * Returns `undefined` on a non-conforming subject. */
function entityFromSubject(subject: string): string | undefined {
  const parts = subject.split(".");
  // Expected shape: device.<plugin>.<device_id>.<entity>.<leaf>
  if (parts.length >= 5 && parts[0] === "device") return parts[3];
  return undefined;
}

function fmtTickTime(ms: number): string {
  const d = new Date(ms);
  // Compact: "Mon 14:32" for ranges over a day, "14:32:05" otherwise.
  // The chart spans both — the renderer doesn't know the range so we
  // pick the form that's least ambiguous in the worst case (showing
  // the day prefix is harmless when zoomed in).
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function fmtRowTime(rfc3339: string): string {
  const d = new Date(rfc3339);
  if (Number.isNaN(d.getTime())) return rfc3339;
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function stringifyValue(v: unknown): string {
  if (v === undefined || v === null) return "—";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  // Struct / list — JSON-stringify but keep one-line.
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}
