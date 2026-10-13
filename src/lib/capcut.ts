import { create } from "zustand";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { api } from "./api";
import { newProjectFromDraft, openProject, refreshLibrary, useLibrary } from "./library";
import { blobUrl, useEditor } from "./store";
import type { CapCutDraft } from "./types";

/** Folders of drafts added by hand, besides the one CapCut keeps them in. */
const FOLDERS_KEY = "nuzky.capcutFolders";

/** What Finish with AI asks the AI panel's agent about a project opened from a draft; the guide tells it how. */
export const FINISH_DRAFT =
  "Finish this draft in my style. It came from CapCut: keep what I edited there, the captions, titles and zooms included, and carry on where I stopped. Tell me what did not come over and what you did about it.";

interface CapCutState {
  /** CapCut's drafts on this computer and in the folders added by hand, newest first; null until looked for. */
  drafts: CapCutDraft[] | null;
  /** The picture CapCut keeps of each draft, by path, with the draft's version it shows. */
  covers: Record<string, { version: number; url: string | null }>;
  showAll: boolean;
  /** The draft being opened. */
  opening: string | null;
  /** The project just opened from a draft, whose list of what did not come over shows. */
  opened: string | null;
}

export const useCapCut = create<CapCutState>(() => ({ drafts: null, covers: {}, showAll: false, opening: null, opened: null }));

const toast = (...args: Parameters<ReturnType<typeof useEditor.getState>["toast"]>) => useEditor.getState().toast(...args);

function folders(): string[] {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(FOLDERS_KEY) ?? "[]");
    return Array.isArray(saved) ? saved.filter((f): f is string => typeof f === "string") : [];
  } catch {
    return [];
  }
}

/** A folder of drafts added by hand, here or in Learn my style, stays on later starts. */
export function rememberDraftFolder(folder: string) {
  localStorage.setItem(FOLDERS_KEY, JSON.stringify([...new Set([...folders(), folder])]));
}

/** CapCut's drafts where CapCut keeps them and in the folders added by hand, newest first. */
export async function listDrafts(): Promise<CapCutDraft[]> {
  const lists = await Promise.all([null, ...folders()].map((folder) => api.styleCapCutDrafts(folder).catch(() => [])));
  const seen = new Set<string>();
  return lists
    .flat()
    .filter((d) => !seen.has(d.path) && !!seen.add(d.path))
    .sort((a, b) => b.modifiedMs - a.modifiedMs);
}

export async function findDrafts() {
  useCapCut.setState({ drafts: await listDrafts() });
}

/** Asks for a folder: one draft opens at once, a folder of drafts joins the list. */
export async function addDraftFolder() {
  const picked = await pickFolder({ directory: true, multiple: false, title: "Choose a CapCut draft, or the folder that holds them" });
  if (typeof picked === "string") await openDraftFolder(picked);
}

export async function openDraftFolder(picked: string) {
  const found = await api.styleCapCutDrafts(picked).catch(() => []);
  if (found.length === 0) return toast({ kind: "info", text: "No CapCut drafts in this folder. Choose a draft's folder, or the folder that holds them." });
  if (found.length === 1) return openDraft(found[0]);
  rememberDraftFolder(picked);
  useCapCut.setState({ showAll: true });
  await findDrafts();
}

/** The project opened from the draft before, if it is still in the library. */
export const projectOf = (draft: CapCutDraft) => useLibrary.getState().projects?.find((p) => p.importedFrom === draft.path && p.state !== "broken");

/** Opens the project made of the draft before, or makes one; what did not come over shows once it is open. */
export function openDraft(draft: CapCutDraft, again = false) {
  const made = projectOf(draft);
  if (made && !again) return openProject(made);
  newProjectFromDraft(
    draft.name,
    async () => {
      useCapCut.setState({ opening: draft.path });
      try {
        return await api.openCapCutDraft(draft.path);
      } finally {
        useCapCut.setState({ opening: null });
      }
    },
    () => {
      const snap = useEditor.getState().snap;
      if (snap?.project.imported?.leftOut.length) useCapCut.setState({ opened: snap.path });
      else toast({ kind: "success", text: `Opened “${draft.name}” from CapCut. Everything came over.` });
      void refreshLibrary();
    },
  );
}

const loading = new Set<string>();

/** Asks for the draft's picture once per version of the draft. */
export function loadCover(draft: CapCutDraft) {
  const key = `${draft.path}:${draft.modifiedMs}`;
  if (useCapCut.getState().covers[draft.path]?.version === draft.modifiedMs || loading.has(key)) return;
  loading.add(key);
  api
    .capCutDraftCover(draft.path)
    .catch(() => null)
    .then((url) => {
      const old = useCapCut.getState().covers[draft.path]?.url;
      if (old?.startsWith("blob:")) URL.revokeObjectURL(old);
      useCapCut.setState((s) => ({ covers: { ...s.covers, [draft.path]: { version: draft.modifiedMs, url: url ? blobUrl(url) : null } } }));
    })
    .finally(() => loading.delete(key));
}

/** "12 clips · 2 captions", for the summary of what came over. */
export function plural(n: number, one: string, many = `${one}s`) {
  return `${n} ${n === 1 ? one : many}`;
}

/** A line of what did not come over, its count apart: "Text shadows (12 times)" is ["Text shadows", "12"]. */
export function leftOutLine(line: string): { what: string; count: string | null; missing: boolean } {
  const counted = /^(.*) \((\d+) (?:times|clips)\)$/.exec(line);
  const what = counted ? counted[1] : line;
  const missing = /^(Missing media|Media Nuzky cannot open|Media that is not a file on this computer|A picture Nuzky cannot show):/.test(what);
  return { what, count: counted ? (line.endsWith("clips)") ? `${counted[2]} clips` : `${counted[2]}×`) : null, missing };
}
