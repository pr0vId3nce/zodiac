// Herd ⇄ Pane screen transition: an animated push/pop instead of a hard
// conditional swap. Two implementations share one hook —
//
//   - View Transitions API (`document.startViewTransition`), used when the
//     browser supports it: the browser itself screenshots old/new DOM and
//     animates between them, driven by CSS in index.css keyed off a
//     `data-nav-direction` attribute.
//   - A manual CSS-transform slide, used everywhere else. This is the real
//     ground-truth implementation, not a stub — same-document View
//     Transitions support is believed to land around Safari 18, while this
//     app's own `deploymentTarget` (ios/project.yml) is iOS 17, so the
//     manual path is load-bearing for the app's stated minimum, not just a
//     legacy nicety.
import { type ReactNode, useEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";

export type Screen = "herd" | "pane";

const TRANSITION_MS = 300;

const supportsViewTransition =
  typeof document !== "undefined" && "startViewTransition" in document;

interface TransitionState {
  from: { screen: Screen; node: ReactNode };
  to: { screen: Screen; node: ReactNode };
  direction: "forward" | "back";
  settled: boolean;
}

export interface NavSlot {
  key: string;
  node: ReactNode;
  className: string;
}

/**
 * `target` is the screen that should be showing right now; `render` builds
 * a screen's content on demand. Returns the slot(s) to actually mount:
 * one when idle, two mid-transition (outgoing + incoming, positioned via
 * className).
 */
export function useScreenNav(
  target: Screen,
  render: (screen: Screen) => ReactNode
): { slots: NavSlot[] } {
  const [screen, setScreen] = useState<Screen>(target);
  const [transition, setTransition] = useState<TransitionState | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // Refreshed on every idle render (see below) — what an outgoing
  // transition freezes on, since by the time the effect below notices
  // `target` changed, the live data behind the old screen (e.g. `pane`)
  // may already be gone (a "back" nav clears `paneId` in the very render
  // that flips `target`, before this effect ever runs).
  const snapshot = useRef<Record<Screen, ReactNode>>({ herd: null, pane: null });

  if (target === screen) {
    snapshot.current[screen] = render(screen);
  }

  useEffect(() => {
    if (target === screen) return;
    const direction: "forward" | "back" = target === "pane" ? "forward" : "back";

    if (supportsViewTransition) {
      document.documentElement.dataset.navDirection = direction;
      (document as any).startViewTransition(() => {
        flushSync(() => setScreen(target));
      });
      return;
    }

    setTransition({
      from: { screen, node: snapshot.current[screen] },
      to: { screen: target, node: render(target) },
      direction,
      settled: false,
    });
    const raf = requestAnimationFrame(() =>
      setTransition((t) => (t ? { ...t, settled: true } : t))
    );
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      setScreen(target);
      setTransition(null);
    }, TRANSITION_MS);
    return () => {
      cancelAnimationFrame(raf);
      if (timer.current) clearTimeout(timer.current);
    };
    // `screen`/`render` deliberately excluded — this effect only cares
    // about `target` changing, not every re-render of its closure.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target]);

  if (!transition) {
    return { slots: [{ key: screen, node: snapshot.current[screen], className: "translate-x-0" }] };
  }
  const { from, to, direction, settled } = transition;
  const exitTo = direction === "forward" ? "-translate-x-full" : "translate-x-full";
  const enterFrom = direction === "forward" ? "translate-x-full" : "-translate-x-full";
  return {
    slots: [
      { key: `${from.screen}-out`, node: from.node, className: settled ? exitTo : "translate-x-0" },
      { key: `${to.screen}-in`, node: to.node, className: settled ? "translate-x-0" : enterFrom },
    ],
  };
}
