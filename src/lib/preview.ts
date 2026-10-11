import { create } from "zustand";
import { getCurrentWindow } from "@tauri-apps/api/window";

const MUTED_KEY = "nuzky.previewMuted";

/** How this computer plays the preview; neither is part of the project or the export. */
export const usePreview = create(() => ({
  muted: localStorage.getItem(MUTED_KEY) === "1",
  full: false,
}));

export function toggleMuted() {
  const muted = !usePreview.getState().muted;
  localStorage.setItem(MUTED_KEY, muted ? "1" : "0");
  usePreview.setState({ muted });
}

/** The UI flows replace its full screen calls, so a run never takes over the screen. */
export const appWindow = getCurrentWindow();
/** A window already in full screen stays in it when the preview leaves. */
let windowWasFull = false;

export async function toggleFullPreview() {
  const full = !usePreview.getState().full;
  usePreview.setState({ full });
  if (full) windowWasFull = await appWindow.isFullscreen();
  // Pressed twice quickly, the window follows the last press.
  if (!windowWasFull) await appWindow.setFullscreen(usePreview.getState().full);
}
