import { useEffect, useRef } from "react";
import { api, errorText, plainError } from "../lib/api";
import { useEditor } from "../lib/store";
import { Button, trapTab } from "./ui";

/** Asks whether to download the speech model an agent needs; the agent waits for the answer. */
export function SpeechModelDialog() {
  const question = useEditor((s) => s.speechModelQuestion);
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (!question || dialog.current?.open) return;
    dialog.current?.showModal();
    dialog.current?.querySelector<HTMLButtonElement>("[data-autofocus]")?.focus();
  }, [question]);
  if (!question) return null;
  const answer = (download: boolean) => {
    // Closed before it unmounts, so focus goes back where it was.
    dialog.current?.close();
    useEditor.setState({ speechModelQuestion: null });
    api.answerSpeechModel(question.id, download).catch((e) => useEditor.getState().toast({ kind: "error", text: plainError(errorText(e)) }));
  };
  return (
    <dialog
      ref={dialog}
      aria-labelledby="speech-model-title"
      aria-describedby="speech-model-text"
      onCancel={(e) => {
        e.preventDefault();
        answer(false);
      }}
      onKeyDown={trapTab}
      className="dialog-in overlay fixed inset-0 m-auto w-[440px] rounded-[20px] p-6 text-fg backdrop:bg-black/45"
    >
      <h2 id="speech-model-title" className="text-[17px] font-semibold">
        Download the speech model?
      </h2>
      <p id="speech-model-text" className="mt-2 text-[13px] text-fg/85">
        The AI needs to know what is said in your video. Nuzky recognises speech on this computer with a speech model, a one-time download of {question.sizeMb} MB.
      </p>
      <div className="mt-6 flex justify-end gap-2">
        <Button pill onClick={() => answer(false)}>
          Not now
        </Button>
        <Button pill variant="primary" data-autofocus onClick={() => answer(true)}>
          Download
        </Button>
      </div>
    </dialog>
  );
}
