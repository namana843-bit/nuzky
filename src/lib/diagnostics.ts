import { api, errorText } from "./api";
import { copyText } from "./clipboard";
import { useEditor } from "./store";

const toast = (kind: "success" | "error", text: string) => useEditor.getState().toast({ kind, text });

/** Puts the version, system, graphics and the end of the log on the clipboard for a bug report. */
export async function copyDiagnostics() {
  try {
    await copyText(await api.diagnostics(), "The clipboard is not available. Open the logs folder and attach nuzky.log instead.");
    toast("success", "Diagnostics copied. Paste them into your bug report.");
  } catch (e) {
    toast("error", `Couldn't copy the diagnostics: ${errorText(e)}`);
  }
}

export function openLogs() {
  api.openLogs().catch((e) => toast("error", `Couldn't open the logs folder: ${errorText(e)}`));
}

/** Copies the diagnostics, then opens the bug report form in the browser, where they are pasted. */
export async function reportBug() {
  await copyDiagnostics();
  await api.openFeedback("bug").catch((e) => toast("error", `Couldn't open the browser: ${errorText(e)}`));
}
