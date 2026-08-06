// The computer's vitals — uptime, battery, CPU, memory — polled from the
// bridge's GET /api/host, rendered as the orrery header's quiet text run:
// dim labels, values in fg, no icons. Stats that don't fit truncate rather
// than wrap.
import { useEffect, useState } from "react";
import { getToken } from "./auth";

interface HostStats {
  uptime: number;
  cpu: number | null;
  mem: { pct: number } | null;
  battery: { pct: number; charging: boolean } | null;
}

function uptimeText(secs: number): string {
  const d = Math.floor(secs / 86_400);
  const h = Math.floor((secs % 86_400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return `${d}d ${h}h`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

export function HostVitals() {
  const [stats, setStats] = useState<HostStats | null>(null);

  useEffect(() => {
    let dead = false;
    const poll = async () => {
      try {
        const token = getToken();
        const res = await fetch("/api/host", {
          headers: token ? { Authorization: `Bearer ${token}` } : {},
        });
        if (!res.ok) return;
        const body = (await res.json()) as HostStats;
        if (!dead) setStats(body);
      } catch {
        /* keep the last numbers through a blip */
      }
    };
    poll();
    const t = setInterval(poll, 5000);
    return () => {
      dead = true;
      clearInterval(t);
    };
  }, []);

  if (!stats) return null;

  const stat = (label: string, value: string, tint?: string) => (
    <span>
      {label} <span className={tint ?? "text-fg"}>{value}</span>
    </span>
  );

  return (
    <div className="flex min-w-0 items-center gap-2.5 overflow-hidden whitespace-nowrap font-mono text-[10px] tabular-nums text-dim">
      {stat("up", uptimeText(stats.uptime))}
      {stats.battery &&
        stat(
          "bat",
          `${stats.battery.pct}%${stats.battery.charging ? "+" : ""}`,
          !stats.battery.charging && stats.battery.pct <= 20 ? "text-red-300" : undefined
        )}
      {stats.cpu !== null && stat("cpu", `${stats.cpu}%`)}
      {stats.mem && stat("mem", `${stats.mem.pct}%`)}
    </div>
  );
}
