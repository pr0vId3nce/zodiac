// Standalone browser/PWA access is paused: the native iOS app is the
// product now, and this bundle's only supported home is its WKWebView.
// A plain browser gets this notice instead of the UI. `?web=1` (sticky
// via localStorage) bypasses it — kept for development and as the
// break-glass path if the app is ever unavailable.
const BYPASS_KEY = "astrolabe-web-override";

export function browserAllowed(): boolean {
  if (new URLSearchParams(location.search).get("web") === "1") {
    localStorage.setItem(BYPASS_KEY, "1");
    return true;
  }
  return localStorage.getItem(BYPASS_KEY) === "1";
}

export function BrowserGate() {
  return (
    <div className="flex min-h-dvh flex-col items-center justify-center gap-4 bg-sky-deep px-8 text-center">
      <div className="flex items-baseline gap-0.5 font-mono text-3xl font-semibold text-white">
        zodiac<span className="animate-pulse text-gold">▍</span>
      </div>
      <p className="max-w-sm font-mono text-sm text-zinc-400">
        the browser version is paused — this machine is watched from the
        zodiac iOS app
      </p>
      <p className="max-w-sm text-xs text-zinc-500">
        open the app and scan this computer's pairing QR (Alt+P on zodiac's
        home page)
      </p>
    </div>
  );
}
