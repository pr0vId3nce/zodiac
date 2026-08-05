import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./ErrorBoundary";
import { initToken } from "./auth";
import { initTheme } from "./theme";
import "./index.css";

// Before anything else mounts: pick up ?th=THEME and ?t=TOKEN from the URL,
// in that order — initToken() scrubs the whole query string, so the theme
// param must be read first. Token first-use: ws.ts's very first connect()
// attempt already has it.
initTheme();
initToken();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </StrictMode>
);
