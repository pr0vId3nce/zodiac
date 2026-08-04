// Two-finger pinch to resize the terminal font, additive to the existing
// +/- buttons (not a replacement). Raw touch events, no library — same
// live-preview / commit-on-release shape as useSwipeBack: a cheap
// transform during the gesture, a real discrete value only on release.
import { useRef } from "react";

const MIN_SCALE = 0.6;
const MAX_SCALE = 1.8;
// Below this, treat it as "didn't really mean to zoom" and skip the commit
// (matches releasing a pinch that barely moved, or a mis-registered pinch).
const COMMIT_EPSILON = 0.02;

function distance(touches: React.TouchList): number {
  const a = touches[0];
  const b = touches[1];
  return Math.hypot(a.clientX - b.clientX, a.clientY - b.clientY);
}

export function usePinchZoom(
  onCommit: (scaleFactor: number) => void,
  preview: (factor: number | null) => void
) {
  const active = useRef(false);
  const startDist = useRef(0);
  const lastFactor = useRef(1);

  const onTouchStart = (e: React.TouchEvent) => {
    if (e.touches.length !== 2) return;
    active.current = true;
    startDist.current = distance(e.touches);
    lastFactor.current = 1;
  };

  const onTouchMove = (e: React.TouchEvent) => {
    if (!active.current || e.touches.length !== 2) return;
    e.preventDefault();
    const ratio = distance(e.touches) / (startDist.current || 1);
    const clamped = Math.max(MIN_SCALE, Math.min(MAX_SCALE, ratio));
    lastFactor.current = clamped;
    preview(clamped);
  };

  const finish = () => {
    if (!active.current) return;
    active.current = false;
    preview(null);
    if (Math.abs(lastFactor.current - 1) > COMMIT_EPSILON) {
      onCommit(lastFactor.current);
    }
  };

  return {
    onTouchStart,
    onTouchMove,
    onTouchEnd: finish,
    onTouchCancel: finish,
  };
}
