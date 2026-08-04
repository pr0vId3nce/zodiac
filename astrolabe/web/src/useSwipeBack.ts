// Edge-swipe-to-dismiss for the Pane view, iOS-style. Raw touch events —
// no gesture library, consistent with this app's hand-rolled UI primitives.
//
// Scope note: this drives the Pane's own transform directly (bypassing
// React state for 60fps tracking) and, on a completed swipe, hands off to
// the normal `nav(null)` back-transition (Nav.tsx) to bring the Herd
// screen in. It does not yet reveal Herd live underneath the finger during
// the drag itself — that "parallax peek" is a follow-up, not implemented
// here; what you get today is direct 1:1 tracking while dragging, then a
// clean handoff to the existing back animation on release.
import { useRef } from "react";
import { hapticTap } from "./native";

const EDGE_ZONE_PX = 24;
const DIRECTION_LOCK_PX = 10;
const COMMIT_FRACTION = 0.35; // of viewport width
const COMMIT_VELOCITY = 0.5; // px/ms
const RELEASE_MS = 200;

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

export function useSwipeBack(onBack: () => void) {
  const el = useRef<HTMLDivElement | null>(null);
  const state = useRef<SwipeState>({
    active: false,
    locked: null,
    startX: 0,
    startY: 0,
    lastX: 0,
    lastT: 0,
    pastThreshold: false,
  });

  const setTransform = (dx: number, animate: boolean) => {
    const node = el.current;
    if (!node) return;
    node.style.transition = animate ? `transform ${RELEASE_MS}ms var(--ease-ios-sheet)` : "none";
    node.style.transform = dx > 0 ? `translateX(${dx}px)` : "";
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
    setTransform(clamped, false);
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
      const node = el.current;
      if (node) {
        node.style.transition = `transform ${RELEASE_MS}ms var(--ease-ios-sheet)`;
        node.style.transform = `translateX(${width}px)`;
      }
      setTimeout(() => {
        onBack();
        if (node) {
          node.style.transition = "none";
          node.style.transform = "";
        }
      }, RELEASE_MS);
    } else {
      setTransform(0, true);
    }
  };

  return {
    ref: el,
    handlers: {
      onTouchStart,
      onTouchMove,
      onTouchEnd: finish,
      onTouchCancel: finish,
    },
  };
}
