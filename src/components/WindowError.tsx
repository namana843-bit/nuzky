import { Component, type ErrorInfo, type ReactNode } from "react";
import { api } from "../lib/api";
import { reportBug, type Say } from "../lib/diagnostics";
import { Button } from "./ui";

const log = (message: string) => void api.logUiError(message).catch(() => {});

window.addEventListener("error", (e) => log(`${e.message} at ${e.filename}:${e.lineno}:${e.colno}`));
window.addEventListener("unhandledrejection", (e) => log(`Unhandled rejection: ${e.reason instanceof Error ? (e.reason.stack ?? e.reason.message) : String(e.reason)}`));

/** A bug while drawing the interface shows this instead of an empty window; the project lives in Rust and stays saved. */
export class WindowErrorBoundary extends Component<{ children: ReactNode }, { failed: boolean; said: Parameters<Say> | null }> {
  state = { failed: false, said: null };

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    log(`The window stopped drawing: ${error.stack ?? error.message}${info.componentStack ?? ""}`);
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="flex h-screen items-center justify-center p-6">
        <div role="alertdialog" aria-labelledby="window-error-title" aria-describedby="window-error-text" className="overlay w-[440px] rounded-[20px] p-6 text-fg">
          <h2 id="window-error-title" className="text-[17px] font-semibold">
            Something went wrong in this window
          </h2>
          <p id="window-error-text" className="mt-3 text-[13px] text-muted">
            Your project is saved. Reload the window to keep editing.
          </p>
          {/* Toasts went with the rest of the window, so how reporting went shows here. */}
          {this.state.said && (
            <p role={this.state.said[0] === "error" ? "alert" : "status"} className={`mt-3 text-[13px] ${this.state.said[0] === "error" ? "text-danger" : "text-fg"}`}>
              {this.state.said[1]}
            </p>
          )}
          <div className="mt-6 flex justify-end gap-2.5">
            <Button pill onClick={() => void reportBug((...said) => this.setState({ said }))}>
              Report a bug…
            </Button>
            <Button pill variant="primary" autoFocus onClick={() => window.location.reload()}>
              Reload
            </Button>
          </div>
        </div>
      </div>
    );
  }
}
