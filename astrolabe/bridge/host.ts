// Vitals for the machine this bridge runs on — uptime, CPU, memory and
// battery — for the phone's title bar. Everything here is best-effort: a
// stat that can't be read on this OS comes back null rather than failing
// the request, since the bar just omits what it doesn't get.
import os from "node:os";
import fs from "node:fs";
import { execFile } from "node:child_process";

export type HostStats = {
  /** Seconds since boot. */
  uptime: number;
  /** Whole-machine CPU busy percentage (0-100), null until sampled. */
  cpu: number | null;
  mem: { used: number; total: number; pct: number } | null;
  battery: { pct: number; charging: boolean } | null;
};

// ---------------------------------------------------------------- cpu

// os.cpus() reports cumulative ticks since boot, so a single reading is
// "average since boot" — useless. Sample on a timer and report the delta
// between the last two, which is the busy fraction over that window.
type CpuSample = { idle: number; total: number };

function cpuSample(): CpuSample {
  let idle = 0;
  let total = 0;
  for (const c of os.cpus()) {
    for (const [kind, ticks] of Object.entries(c.times)) {
      total += ticks;
      if (kind === "idle") idle += ticks;
    }
  }
  return { idle, total };
}

let prevCpu = cpuSample();
let cpuPct: number | null = null;

const cpuTimer = setInterval(() => {
  const now = cpuSample();
  const idle = now.idle - prevCpu.idle;
  const total = now.total - prevCpu.total;
  prevCpu = now;
  if (total > 0) cpuPct = Math.max(0, Math.min(100, Math.round((1 - idle / total) * 100)));
}, 3000);
// Never hold the process open for a stat nobody is reading.
cpuTimer.unref();

// ---------------------------------------------------------------- helpers

function run(cmd: string, args: string[], timeout = 2000): Promise<string | null> {
  return new Promise((resolve) => {
    execFile(cmd, args, { timeout, encoding: "utf8" }, (err, stdout) =>
      resolve(err ? null : stdout)
    );
  });
}

// ---------------------------------------------------------------- memory

/** Linux: MemAvailable is the kernel's own "how much can you still use". */
function memLinux(): HostStats["mem"] {
  try {
    const info = fs.readFileSync("/proc/meminfo", "utf8");
    const kb = (key: string) =>
      Number(info.match(new RegExp(`^${key}:\\s+(\\d+) kB`, "m"))?.[1] ?? NaN);
    const total = kb("MemTotal") * 1024;
    const avail = kb("MemAvailable") * 1024;
    if (!Number.isFinite(total) || !Number.isFinite(avail)) return null;
    const used = total - avail;
    return { used, total, pct: Math.round((used / total) * 100) };
  } catch {
    return null;
  }
}

/** macOS: os.freemem() counts only genuinely free pages, so it reads as
 *  "95% used" on any healthy Mac. Activity Monitor's "Memory Used" is
 *  active + wired + compressed, which is what vm_stat gives us. */
async function memMac(): Promise<HostStats["mem"]> {
  const out = await run("/usr/bin/vm_stat", []);
  if (!out) return null;
  const pageSize = Number(out.match(/page size of (\d+) bytes/)?.[1] ?? 4096);
  const pages = (label: string) =>
    Number(out.match(new RegExp(`${label}:\\s+(\\d+)\\.`))?.[1] ?? NaN);
  const active = pages("Pages active");
  const wired = pages("Pages wired down");
  const compressed = pages("Pages occupied by compressor");
  if (![active, wired, compressed].every(Number.isFinite)) return null;
  const total = os.totalmem();
  const used = (active + wired + compressed) * pageSize;
  return { used, total, pct: Math.round((used / total) * 100) };
}

// ---------------------------------------------------------------- battery

/** Linux: the first real battery under /sys/class/power_supply. */
function batteryLinux(): HostStats["battery"] {
  try {
    const base = "/sys/class/power_supply";
    for (const name of fs.readdirSync(base)) {
      const dir = `${base}/${name}`;
      const type = fs.readFileSync(`${dir}/type`, "utf8").trim();
      if (type !== "Battery") continue;
      const pct = Number(fs.readFileSync(`${dir}/capacity`, "utf8").trim());
      const status = fs.readFileSync(`${dir}/status`, "utf8").trim();
      if (!Number.isFinite(pct)) continue;
      return { pct, charging: status === "Charging" || status === "Full" };
    }
  } catch {
    /* desktops have no battery — that's a null, not an error */
  }
  return null;
}

/** macOS: `pmset -g batt` prints "-InternalBattery-0 (id=…) 87%; charging;"
 *  — and nothing resembling that on a desktop Mac. */
async function batteryMac(): Promise<HostStats["battery"]> {
  const out = await run("/usr/bin/pmset", ["-g", "batt"]);
  if (!out) return null;
  const m = out.match(/(\d+)%;\s*([a-z ]+);/);
  if (!m) return null;
  const state = m[2].trim();
  return {
    pct: Number(m[1]),
    charging: state === "charging" || state === "charged" || state === "AC attached",
  };
}

// ---------------------------------------------------------------- public

let cache: { at: number; stats: HostStats } | null = null;
const TTL_MS = 3000;

/** Current vitals, cached briefly so several phones (or a fast poll)
 *  don't shell out to vm_stat/pmset on every request. */
export async function hostStats(): Promise<HostStats> {
  if (cache && Date.now() - cache.at < TTL_MS) return cache.stats;
  if (cpuPct === null) {
    // Asked before the first timer tick — measure against whatever
    // baseline we have (process start) rather than reporting nothing.
    const now = cpuSample();
    const idle = now.idle - prevCpu.idle;
    const total = now.total - prevCpu.total;
    prevCpu = now;
    if (total > 0) cpuPct = Math.max(0, Math.min(100, Math.round((1 - idle / total) * 100)));
  }
  const mac = process.platform === "darwin";
  const [mem, battery] = await Promise.all([
    mac ? memMac() : Promise.resolve(memLinux()),
    mac ? batteryMac() : Promise.resolve(batteryLinux()),
  ]);
  const stats: HostStats = { uptime: Math.round(os.uptime()), cpu: cpuPct, mem, battery };
  cache = { at: Date.now(), stats };
  return stats;
}
