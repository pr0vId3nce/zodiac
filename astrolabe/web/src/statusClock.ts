// "needs you · 4m" wants to know when a pane entered its current status,
// but the server doesn't timestamp status flips. This module-level clock
// watches statuses as states stream in and records transitions it actually
// sees. A pane's first sighting gets no timestamp — it may have been
// sitting in that status for hours — so callers render no duration until a
// real transition happens on this client's watch. Module-level on purpose:
// Herd and Pane both consult it, and it survives screen changes.

const seen = new Map<number, { status: string; since: number; known: boolean }>();

/** Record the pane's current status; returns when it entered it, or null
    if this client never saw the transition. */
export function observeStatus(id: number, status: string): number | null {
  const rec = seen.get(id);
  if (!rec || rec.status !== status) {
    seen.set(id, { status, since: Date.now(), known: !!rec });
    return rec ? Date.now() : null;
  }
  return rec.known ? rec.since : null;
}
