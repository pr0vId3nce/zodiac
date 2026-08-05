// Edge-swipe-to-dismiss for the Pane view, iOS-style. Raw touch events —
// no gesture library, consistent with this app's hand-rolled UI primitives.
//
// Reveals a backdrop (Herd, passed in by Pane.tsx) live underneath the
// finger as you drag — same parallax shape as a UINavigationController
// pop: the view underneath sits pulled back and dimmed at rest, and
// settles to its resting position as the front view slides away. On a
// completed swipe, `onCommit` fires once the release animation finishes;
// the caller (App.tsx) uses that to sync Nav's state without replaying
// its own transition on top of a screen that's already fully revealed.
import { useRef } from "react";
import { hapticTap } from "./native";

const EDGE_ZONE_PX = 24;
const DIRECTION_LOCK_PX = 10;
const COMMIT_FRACTION = 0.35; // of viewport width
const COMMIT_VELOCITY = 0.5; // px/ms
const RELEASE_MS = 200;
// How far back (as a % translateX) the backdrop sits before a swipe
// starts revealing it, and how dark its resting scrim is.
const BACKDROP_REST_PCT = 25;
const SCRIM_REST_OPACITY = 0.35;

interface SwipeState {
  active: boolean;
  locked: "horizontal" | "vertical" | null;
  startX: number;
  startY: number;
  lastX: number;
  lastT: number;
  /** Fires once per drag, the instant it first crosses the commit
      distance — a preview of "letting go now would go back", same idea
      as iOS's own edge-swipe haptic tick. */
  pastThreshold: boolean;
}

export function useSwipeBack(onCommit: () => void) {
  const el = useRef<HTMLDivElement | null>(null);
  const backdropEl = useRef<HTMLDivElement | null>(null);
  const scrimEl = useRef<HTMLDivElement | null>(null);
  const state = useRef<SwipeState>({
    active: false,
    locked: null,
    startX: 0,
    startY: 0,
    lastX: 0,
    lastT: 0,
    pastThreshold: false,
  });

  const setProgress = (dx: number, animate: boolean) => {
    const node = el.current;
    const backdrop = backdropEl.current;
    const scrim = scrimEl.current;
    const width = window.innerWidth || 1;
    const transition = animate
      ? `transform ${RELEASE_MS}ms var(--ease-ios-sheet)`
      : "none";
    if (node) {
      node.style.transition = transition;
      node.style.transform = dx > 0 ? `translateX(${dx}px)` : "";
    }
    const reveal = Math.max(0, Math.min(1, dx / width));
    if (backdrop) {
      backdrop.style.transition = transition;
      backdrop.style.transform = `translateX(${-BACKDROP_REST_PCT * (1 - reveal)}%)`;
    }
    if (scrim) {
      scrim.style.transition = animate ? `opacity ${RELEASE_MS}ms var(--ease-ios-sheet)` : "none";
      scrim.style.opacity = String(SCRIM_REST_OPACITY * (1 - reveal));
    }
  };

  const onTouchStart = (e: React.TouchEvent) => {
    const t = e.touches[0];
    if (!t || t.clientX > EDGE_ZONE_PX) return;
    state.current = {
      active: true,
      locked: null,
      startX: t.clientX,
      startY: t.clientY,
      lastX: t.clientX,
      lastT: Date.now(),
      pastThreshold: false,
    };
  };

  const onTouchMove = (e: React.TouchEvent) => {
    const s = state.current;
    if (!s.active) return;
    const t = e.touches[0];
    if (!t) return;
    const dx = t.clientX - s.startX;
    const dy = t.clientY - s.startY;
    if (!s.locked) {
      if (Math.abs(dx) < DIRECTION_LOCK_PX && Math.abs(dy) < DIRECTION_LOCK_PX) return;
      s.locked = Math.abs(dx) > Math.abs(dy) ? "horizontal" : "vertical";
      if (s.locked === "vertical") {
        s.active = false;
        return;
      }
    }
    if (s.locked !== "horizontal") return;
    e.preventDefault();
    s.lastX = t.clientX;
    s.lastT = Date.now();
    const clamped = Math.max(0, dx);
    setProgress(clamped, false);
    const past = clamped > window.innerWidth * COMMIT_FRACTION;
    if (past && !s.pastThreshold) hapticTap("impact", el.current);
    s.pastThreshold = past;
  };

  const finish = () => {
    const s = state.current;
    if (!s.active || s.locked !== "horizontal") {
      state.current.active = false;
      return;
    }
    s.active = false;
    const dx = Math.max(0, s.lastX - s.startX);
    const dt = Math.max(1, Date.now() - s.lastT);
    const velocity = dx / dt;
    const width = window.innerWidth;
    const commit = dx > width * COMMIT_FRACTION || velocity > COMMIT_VELOCITY;

    if (commit) {
      setProgress(width, true);
      setTimeout(onCommit, RELEASE_MS);
    } else {
      setProgress(0, true);
    }
  };

  return {
    ref: el,
    backdropRef: backdropEl,
    scrimRef: scrimEl,
    restTransform: `translateX(-${BACKDROP_REST_PCT}%)`,
    restScrimOpacity: SCRIM_REST_OPACITY,
    handlers: {
      onTouchStart,
      onTouchMove,
      onTouchEnd: finish,
      onTouchCancel: finish,
    },
  };
}
