import { useEffect, useRef, type KeyboardEvent } from "react";
import { Check, X } from "lucide-react";
import { SETUP_STEPS, useSetup } from "../../lib/setup";
import { Button, IconButton, trapTab } from "../ui";
import { STEPS } from "./steps";

const LAST = SETUP_STEPS.length - 1;

/** Get set up: the setup steps beside each other with whether each is done or skipped; it continues where it was left. */
export function SetupDialog() {
  const { open, states, current, closeSetup, showStep, skipStep } = useSetup();
  const dialog = useRef<HTMLDialogElement>(null);
  const rail = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const d = dialog.current;
    if (!d) return;
    // Closing gives focus back to what opened it.
    if (!open) return void (d.open && d.close());
    if (d.open) return;
    d.showModal();
    // Never Skip: a stray Enter or Space skips nothing.
    rail.current?.querySelector<HTMLElement>("[aria-selected=true]")?.focus();
  }, [open]);

  const index = SETUP_STEPS.indexOf(current);
  const done = SETUP_STEPS.filter((step) => states[step] === "done").length;
  const { title, Body } = STEPS[current];

  const railKeys = (e: KeyboardEvent<HTMLDivElement>) => {
    const to = { ArrowUp: index - 1, ArrowDown: index + 1, Home: 0, End: LAST }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    const step = SETUP_STEPS[Math.max(0, Math.min(LAST, to))];
    showStep(step);
    e.currentTarget.querySelector<HTMLElement>(`#setup-tab-${step}`)?.focus();
  };

  return (
    <dialog
      ref={dialog}
      aria-labelledby="setup-title"
      aria-describedby="setup-count"
      onCancel={(e) => {
        e.preventDefault();
        closeSetup();
      }}
      onKeyDown={(e) => {
        // The editor's and the home screen's keys (Space plays, typing searches) wait while it is open.
        e.stopPropagation();
        trapTab(e);
      }}
      className="dialog-in overlay fixed inset-0 m-auto h-[420px] max-h-[calc(100vh-48px)] w-[640px] rounded-[20px] p-6 text-fg outline-none backdrop:bg-black/45"
    >
      <div className="flex h-full flex-col">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 id="setup-title" className="text-[17px] font-semibold leading-[22px] tracking-[-0.025em]">
              Get set up
            </h2>
            <p id="setup-count" className="tabular mt-0.5 text-[13px] text-muted">
              {done} of {SETUP_STEPS.length} done
            </p>
          </div>
          <IconButton label="Close" round className="-mr-1.5 -mt-0.5 h-7 w-7" onClick={closeSetup}>
            <X size={16} />
          </IconButton>
        </div>
        <div className="mt-5 flex min-h-0 flex-1">
          <div ref={rail} role="tablist" aria-orientation="vertical" aria-label="Setup steps" onKeyDown={railKeys} className="flex w-[196px] shrink-0 flex-col gap-0.5 overflow-y-auto">
            {SETUP_STEPS.map((step, i) => {
              const state = states[step];
              const on = step === current;
              return (
                <button
                  key={step}
                  type="button"
                  role="tab"
                  id={`setup-tab-${step}`}
                  aria-selected={on}
                  aria-controls="setup-panel"
                  tabIndex={on ? 0 : -1}
                  onClick={() => showStep(step)}
                  className={`flex shrink-0 items-start gap-2.5 rounded-lg px-2.5 py-1.5 text-left transition-colors duration-[120ms] ease-out ${on ? "bg-white/[.08]" : "hover:bg-white/[.05]"}`}
                >
                  <span
                    className={`tabular flex size-5 shrink-0 items-center justify-center rounded-full text-[11px] font-semibold ${
                      state === "done" ? "bg-accent-strong text-white" : state === "skipped" ? "text-muted shadow-[inset_0_0_0_1.5px_rgb(255_255_255/.25)]" : "bg-white/[.09] text-fg"
                    }`}
                  >
                    {state === "done" ? <Check size={12} strokeWidth={3} /> : i + 1}
                  </span>
                  <span className="min-w-0 pt-px">
                    <span className={`block text-[13px] text-fg ${on ? "font-medium" : ""}`}>{STEPS[step].title}</span>
                    {state && <span className="block text-[12px] text-muted">{state === "done" ? "Done" : "Skipped"}</span>}
                  </span>
                </button>
              );
            })}
          </div>
          <div className="mx-4 w-px shrink-0 bg-white/[.07]" />
          <div role="tabpanel" id="setup-panel" aria-labelledby={`setup-tab-${current}`} className="flex min-w-0 flex-1 flex-col">
            <h3 className="text-[15px] font-semibold">{title}</h3>
            <Body />
            <footer className="mt-4 flex justify-end gap-2">
              {!states[current] ? (
                <Button pill onClick={() => skipStep(current)}>
                  Skip
                </Button>
              ) : index < LAST ? (
                <Button pill variant="primary" onClick={() => showStep(SETUP_STEPS[index + 1])}>
                  Next
                </Button>
              ) : (
                <Button pill variant="primary" onClick={closeSetup}>
                  Done
                </Button>
              )}
            </footer>
          </div>
        </div>
      </div>
    </dialog>
  );
}
