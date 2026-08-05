import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./ErrorBoundary";
import { initToken } from "./auth";
import "./index.css";

// Before anything else mounts: pick up ?t=TOKEN from a magic link, if
// present, so ws.ts's very first connect() attempt already has it.
initToken();

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </StrictMode>
);
