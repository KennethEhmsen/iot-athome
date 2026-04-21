import { Component, type ReactNode } from "react";

interface State {
  err: Error | null;
}

export class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  override state: State = { err: null };

  static getDerivedStateFromError(err: Error) {
    return { err };
  }

  override componentDidCatch(err: Error, info: unknown) {
    console.error("[ErrorBoundary]", err, info);
  }

  override render() {
    if (this.state.err) {
      return (
        <div className="m-6 p-4 rounded-lg border border-rose-500/40 bg-rose-500/10 text-rose-200">
          <h2 className="font-semibold mb-2">{this.state.err.name}</h2>
          <p className="text-sm">{this.state.err.message}</p>
          <pre className="mt-3 text-xs text-rose-300/70 whitespace-pre-wrap">
            {this.state.err.stack}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}
