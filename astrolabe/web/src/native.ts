// Haptic-feel shim. There is no way to reach real haptics from iOS
// Safari/WKWebView today — the Vibration API has never been implemented
// by WebKit, in-browser or inside a WKWebView — so this always falls back
// to a visual "impulse" for now. `isNativeShell()` is the forward hook: if
// a future native wrapper ever injects a `webkit.messageHandlers.astrolabe`
// bridge (none does yet — confirmed WebView.swift has no
// WKScriptMessageHandler), this starts routing through it with zero other
// call-site changes.
export type HapticKind = "tap" | "impact" | "success" | "warning" | "selection";

export function isNativeShell(): boolean {
  return typeof window !== "undefined" && !!(window as any).webkit?.messageHandlers?.astrolabe;
}

/**
 * Fire a haptic-feeling response to a high-signal interaction. `el` is the
 * element to visually pulse when there's no native bridge to actually
 * vibrate — reserved for moments worth the attention (message sent, a
 * viewed pane flips to needs_input, swipe-back commits, KeyPad taps,
 * slash-command picks), not every tap.
 */
export function hapticTap(kind: HapticKind = "tap", el?: HTMLElement | null) {
  if (isNativeShell()) {
    (window as any).webkit.messageHandlers.astrolabe.postMessage({ type: "haptic", kind });
    return;
  }
  if (!el) return;
  el.classList.remove("haptic-pulse");
  void el.offsetWidth; // force a reflow so the animation restarts on rapid repeats
  el.classList.add("haptic-pulse");
}
