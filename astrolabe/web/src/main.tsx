import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./ErrorBoundary";
import { initToken } from "./auth";
import { initTheme } from "./theme";
import { isNativeShell } from "./native";
import { BrowserGate, browserAllowed } from "./BrowserGate";
import "./index.css";

// Before anything else mounts: pick up ?th=THEME and ?t=TOKEN from the URL,
// in that order — initToken() scrubs the whole query string, so the theme
// param must be read first. Token first-use: ws.ts's very first connect()
// attempt already has it.
initTheme();
// Read (and persist) the ?web=1 override before initToken() scrubs the
// query string away.
const webOverride = browserAllowed();
initToken();

// Native shell (WKWebView with the haptics bridge) gets the app; a plain
// browser gets the paused notice unless ?web=1 lets it through — see
// BrowserGate.tsx. Decided after initToken(), which scrubs the query, so
// a magic link still stores its token either way.
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      {isNativeShell() || webOverride ? <App /> : <BrowserGate />}
    </ErrorBoundary>
  </StrictMode>
);
