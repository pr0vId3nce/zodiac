// A runtime exception anywhere in the tree used to blank the whole app
// with no recovery UI. Error boundaries have no hook equivalent — this has
// to be a class component.
import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}

interface State {
  error: Error | null;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("astrolabe: unhandled error in the UI:", error, info.componentStack);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
        <div className="text-body font-semibold text-zinc-200">something broke</div>
        <div className="max-w-xs text-caption text-zinc-500">{this.state.error.message}</div>
        <button
          onClick={() => location.reload()}
          className="mt-2 rounded-lg border border-card-edge bg-card px-4 py-2 text-sm font-medium text-zinc-200 active:scale-[0.97]"
        >
          Reload
        </button>
      </div>
    );
  }
}
