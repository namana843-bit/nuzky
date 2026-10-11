import { useEffect, useRef } from "react";
import { api, errorText, plainError } from "../lib/api";
import { useEditor } from "../lib/store";
import { Button, trapTab } from "./ui";

/** Asks whether to download the speech model an agent needs; the agent waits for the answer. */
export function SpeechModelDialog() {
  const question = useEditor((s) => s.speechModelQuestion);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = dialog.current;
    if (!d) return;
    // Closing, also when the agent no longer waits, gives focus back to where it was.
    if (!question) return void (d.open && d.close());
    if (d.open) return;
    d.showModal();
    // It opens unasked, so the dialog takes focus rather than a button: a Space typed meanwhile starts nothing.
    d.focus();
  }, [question]);
  const answer = (download: boolean) => {
    if (!question) return;
    useEditor.setState({ speechModelQuestion: null });
    api.answerSpeechModel(question.id, download).catch((e) => useEditor.getState().toast({ kind: "error", text: plainError(errorText(e)) }));
  };
  return (
    <dialog
      ref={dialog}
      tabIndex={-1}
      aria-labelledby="speech-model-title"
      aria-describedby="speech-model-text"
      onCancel={(e) => {
        e.preventDefault();
        answer(false);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" && e.target === e.currentTarget) answer(true);
        trapTab(e);
      }}
      className="dialog-in overlay fixed inset-0 m-auto w-[440px] rounded-[20px] p-6 text-fg outline-none backdrop:bg-black/45"
    >
      {question && (
        <>
          <h2 id="speech-model-title" className="text-[17px] font-semibold leading-[22px] tracking-[-0.025em]">
            Download the speech model?
          </h2>
          <p id="speech-model-text" className="mt-2 text-[13px] text-fg/85">
            The AI needs to know what is said in your video. Nuzky recognises speech on this computer with a speech model, a one-time download of {question.sizeMb} MB. Once it is
            downloaded, the AI goes on by itself; with Not now, it goes on without a transcript.
          </p>
          <div className="mt-6 flex justify-end gap-2">
            <Button pill onClick={() => answer(false)}>
              Not now
            </Button>
            <Button pill variant="primary" onClick={() => answer(true)}>
              Download
            </Button>
          </div>
        </>
      )}
    </dialog>
  );
}
