import { create } from "zustand";
import { api, errorText, plainError } from "./api";
import { useEditor } from "./store";
import type { ChatGptStatus } from "./types";

/**
 * The ChatGPT sign-in as the ChatGPT dialog shows it. The backend keeps the tokens; this knows only
 * who is signed in. A sign-in goes on while the dialog is closed and reports in a toast then.
 */
type ChatGpt = {
  open: boolean;
  /** Null until the first check answers. */
  status: ChatGptStatus | null;
  checking: boolean;
  action: "signingIn" | "signingOut" | null;
  /** What went wrong last, in plain words. */
  error: string | null;
};

export const useChatGpt = create<ChatGpt>(() => ({ open: false, status: null, checking: false, action: null, error: null }));

/** Each sign-in's number; a later one, or Cancel, makes an earlier one's answer stale. */
let attempt = 0;

export function openChatGpt() {
  useChatGpt.setState({ open: true, error: null });
  if (!useChatGpt.getState().action) void check();
}

async function check() {
  useChatGpt.setState({ checking: true });
  try {
    useChatGpt.setState({ status: await api.chatgptStatus() });
  } catch (e) {
    useChatGpt.setState({ error: plainError(errorText(e)) });
  } finally {
    useChatGpt.setState({ checking: false });
  }
}

export async function signIn(anotherAccount = false) {
  const mine = ++attempt;
  useChatGpt.setState({ action: "signingIn", error: null });
  try {
    const status = await api.chatgptSignIn(anotherAccount);
    if (mine !== attempt) return;
    useChatGpt.setState({ status, action: null });
    if (!useChatGpt.getState().open && status.state === "signedIn")
      useEditor.getState().toast(
        status.plan
          ? { kind: "success", text: `Signed in to ChatGPT${status.email ? ` as ${status.email}` : ""}.` }
          : { kind: "warning", text: "Signed in, but Nuzky can't use your ChatGPT plan yet. Open ChatGPT account… in the AI panel to allow it." },
      );
  } catch (e) {
    if (mine !== attempt) return;
    const text = errorText(e);
    useChatGpt.setState({ action: null, error: text.startsWith("CANCELLED") ? null : plainError(text) });
    if (!useChatGpt.getState().open && !text.startsWith("CANCELLED")) useEditor.getState().toast({ kind: "error", text: plainError(text) });
  }
}

export function cancelSignIn() {
  attempt++;
  useChatGpt.setState({ action: null });
  void api.chatgptCancelSignIn();
}

export async function signOut() {
  useChatGpt.setState({ action: "signingOut", error: null });
  try {
    useChatGpt.setState({ status: await api.chatgptSignOut() });
  } catch (e) {
    const text = plainError(errorText(e));
    useChatGpt.setState({ error: text });
    if (!useChatGpt.getState().open) useEditor.getState().toast({ kind: "error", text: `Signing out of ChatGPT failed. ${text}` });
  } finally {
    useChatGpt.setState({ action: null });
  }
}
