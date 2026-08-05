// The bridge's shared-secret token (ASTROLABE_TOKEN on the bridge host).
// Delivered once via a magic link — `https://host:7979/#/?t=TOKEN` — read
// here on boot, saved to localStorage, and scrubbed from the visible URL
// so it doesn't linger in the address bar/any share sheet after the fact.
// Every reload after that reads it back from storage; the WebSocket is the
// only thing that needs it (the web app never calls /api/* directly — only
// the iOS shell's Bridge.swift does).
const STORAGE_KEY = "astrolabe-token";

export function initToken(): void {
  const params = new URLSearchParams(location.search);
  const t = params.get("t");
  if (!t) return;
  localStorage.setItem(STORAGE_KEY, t);
  history.replaceState(null, "", location.pathname + location.hash);
}

export function getToken(): string | null {
  return localStorage.getItem(STORAGE_KEY);
}
