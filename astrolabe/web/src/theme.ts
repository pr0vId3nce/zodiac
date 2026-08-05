// Theme plumbing — "night" (the default night-sky look), "oled-orange",
// "oled-green". The palettes themselves live in index.css as
// `html[data-theme=…]` CSS-variable override blocks; everything here just
// decides which one is active.
//
// The iOS shell passes `?th=<name>` alongside the pairing token (and pokes
// `document.documentElement.dataset.theme` live when the user switches
// themes with a WebView already open — see WebView.swift). Browser visits
// keep whatever the last visit stored. Must run before auth.ts's
// initToken(), which scrubs the entire query string.
const KEY = "astrolabe-theme";

export function initTheme(): void {
  const th = new URLSearchParams(location.search).get("th");
  if (th) localStorage.setItem(KEY, th);
  const cur = localStorage.getItem(KEY);
  if (cur && cur !== "night") document.documentElement.dataset.theme = cur;
}
