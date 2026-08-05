// The computer's vitals — uptime, battery, CPU, memory — polled from the
// bridge's GET /api/host. Shown in the herd header when running inside the
// native iOS shell, where the old "zodiac ✶ session" branding row would
// otherwise repeat what the native title bar already says. One row, always:
// stats that don't fit truncate rather than wrap.
import { useEffect, useState } from "react";
import { BatteryCharging, BatteryMedium, Cpu, MemoryStick, Power } from "lucide-react";
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

  const chip = (icon: React.ReactNode, text: string, tint?: string) => (
    <span className={`flex items-center gap-1 ${tint ?? "text-zinc-400"}`}>
      {icon}
      <span className="font-mono text-caption tabular-nums">{text}</span>
    </span>
  );

  return (
    <div className="flex min-w-0 items-center gap-3 overflow-hidden whitespace-nowrap">
      {chip(<Power className="h-3 w-3" />, uptimeText(stats.uptime))}
      {stats.battery &&
        chip(
          stats.battery.charging ? (
            <BatteryCharging className="h-3 w-3" />
          ) : (
            <BatteryMedium className="h-3 w-3" />
          ),
          `${stats.battery.pct}%`,
          !stats.battery.charging && stats.battery.pct <= 20 ? "text-red-400" : undefined
        )}
      {stats.cpu !== null && chip(<Cpu className="h-3 w-3" />, `${stats.cpu}%`)}
      {stats.mem && chip(<MemoryStick className="h-3 w-3" />, `${stats.mem.pct}%`)}
    </div>
  );
}
