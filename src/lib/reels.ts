import { create } from "zustand";
import { api, errorText, plainError } from "./api";
import { send, sendBlocked, useAgent } from "./agent";
import { useDock } from "./dock";
import { openProject, refreshLibrary } from "./library";
import { aiLocked, currentEpoch, projectDuration, useEditor, whenIdle } from "./store";
import { US } from "./time";
import type { JobEvent, MadeReel, Project, ReelCandidate, ReelFraming, TranscriptWord } from "./types";

/** From this length on the Transcript tab offers Reels from a long video. */
export const LONG_VIDEO_US = 180 * US;
/** The longest reel propose_reels suggests; a longer one gets a warning but can still be made. */
export const REEL_LIMIT_US = 60 * US;

/** What Find reels asks the AI panel's agent; the guide it reads tells it how. */
export const FIND_REELS =
  "Find the moments in this video that work as reels on their own and propose them for me to pick. Transcribe it first if it has no transcript. Don't change the video itself.";

const FRAMING_KEY = "nuzky.reelFraming";

interface ReelsState {
  open: boolean;
  /** The candidate in the preview and the trim pane. */
  current: string | null;
  framing: ReelFraming;
  /** The playhead and clip selection from before, given back on close. */
  before: { timeUs: number; selection: string[] } | null;
  /** Bounds asked for and not confirmed yet, so the trim pane shows them in the same frame. */
  trim: { id: string; from: number; to: number } | null;
  /** Find reels sent its message and the agent is answering it. */
  finding: boolean;
  /** Find reels has finished at least once in this session, so an empty list means nothing was found. */
  searched: boolean;
  /** The make job this mode started. */
  job: string | null;
  /** Why the last make failed, until another starts. */
  failed: string | null;
  /** The video's words as they play now, read once for the list and the trim pane. */
  transcript: { words: TranscriptWord[] | null; error: string | null };
}

export const useReels = create<ReelsState>(() => ({
  open: false,
  current: null,
  framing: localStorage.getItem(FRAMING_KEY) === "blur" ? "blur" : "crop",
  before: null,
  trim: null,
  finding: false,
  searched: false,
  job: null,
  failed: null,
  transcript: { words: null, error: null },
}));

/** Best first, the rest in the order they are said. */
export function candidates(project: Project | undefined): ReelCandidate[] {
  return [...(project?.reelCandidates ?? [])].sort((a, b) => b.score - a.score || a.startUs - b.startUs);
}

export const candidateOf = (project: Project | undefined, id: string | null) => project?.reelCandidates?.find((c) => c.id === id);

/** A cut made since the reel was found moved its words, so making it would cut others. */
export const moved = (c: ReelCandidate, words: TranscriptWord[] | null) =>
  !!words && c.status !== "made" && !(words[c.from]?.startUs === c.startUs && words[c.to]?.endUs === c.endUs);

/** Opens the mode on the best candidate; the playhead and selection come back on close. */
export function openReels() {
  const { snap, playing, timeUs, selection } = useEditor.getState();
  if (!snap) return;
  if (playing) void api.pause();
  const before = useReels.getState().before ?? { timeUs, selection };
  useReels.setState({ open: true, before, failed: null });
  useEditor.getState().select([]);
  showBest(snap.project);
  // Says whether media files of the video are missing.
  void refreshLibrary();
}

/** Shows the best reel not rejected, when the one shown is gone or there was none. */
function showBest(project: Project) {
  const list = candidates(project);
  const best = list.find((c) => c.status !== "rejected") ?? list[0];
  if (best) choose(best.id);
}

export function closeReels() {
  const before = useReels.getState().before;
  if (useEditor.getState().playing) void api.pause();
  useReels.setState({ open: false, before: null, trim: null });
  if (before) {
    useEditor.getState().seek(before.timeUs);
    useEditor.getState().select(before.selection);
  }
}

/** Shows a candidate in the preview, from its start. */
export function choose(id: string) {
  const c = candidateOf(useEditor.getState().snap?.project, id);
  useReels.setState({ current: id, trim: null });
  if (c) useEditor.getState().seek(c.startUs);
}

/** Set when playback stopped at the current reel's end, so Play starts it again. */
let atEnd = false;

/** Plays the current reel to its end, from its start when it stopped there. */
export function toggleReelPlay() {
  const { playing, snap, seek, togglePlay } = useEditor.getState();
  const c = candidateOf(snap?.project, useReels.getState().current);
  if (!playing && c && atEnd) seek(c.startUs);
  atEnd = false;
  togglePlay();
}

/** The last seconds of the current reel, to hear how it ends. */
export function playEnding() {
  const { playing, snap, seek } = useEditor.getState();
  const c = candidateOf(snap?.project, useReels.getState().current);
  if (!c) return;
  seek(Math.max(c.startUs, c.endUs - 3 * US));
  atEnd = false;
  if (!playing) void api.play();
}

// Playback stops where the reel ends, once it gets there from inside it.
useEditor.subscribe((s, prev) => {
  if (!useReels.getState().open || !s.playing) return;
  const c = candidateOf(s.snap?.project, useReels.getState().current);
  if (c && prev.timeUs < c.endUs && s.timeUs >= c.endUs) {
    atEnd = true;
    void api.pause();
    useEditor.getState().seek(c.endUs);
  }
});

// The agent's first reels show at once, and so does another one when the reel shown goes.
useEditor.subscribe((s) => {
  const { open, current } = useReels.getState();
  if (open && s.snap && !candidateOf(s.snap.project, current)) showBest(s.snap.project);
});

// Another project opened: its editor starts on the video.
useEditor.subscribe((s, prev) => {
  if (prev.snap && s.snap && s.snap.sessionEpoch !== prev.snap.sessionEpoch && useReels.getState().open)
    useReels.setState({ open: false, before: null, trim: null, current: null, searched: false, failed: null });
});

/** Changes one candidate as one undo step, built on the latest project; a made one stays as it is. */
function update(id: string, change: (c: ReelCandidate) => ReelCandidate, coalesce?: string) {
  return useEditor.getState().edit((project) => {
    const c = candidateOf(project, id);
    return c && c.status !== "made" ? { type: "updateReelCandidate", candidate: change(c) } : null;
  }, coalesce);
}

export const setPicked = (id: string, picked: boolean) => update(id, (c) => ({ ...c, status: picked ? "picked" : "proposed" }));
export const setRejected = (id: string, rejected: boolean) => update(id, (c) => ({ ...c, status: rejected ? "rejected" : "proposed" }));

/** Checks every proposed candidate as one undo step. */
export function pickAll() {
  return useEditor.getState().edit((project) => {
    const open = (project.reelCandidates ?? []).filter((c) => c.status === "proposed");
    return open.map((c) => ({ type: "updateReelCandidate", candidate: { ...c, status: "picked" } }));
  });
}

let boundsAsked = 0;

/**
 * Moves the reel to start at word `from` and end at word `to`. The engine measures how long it is once cut; a burst
 * of keys is one undo step.
 */
export async function setBounds(id: string, from: number, to: number) {
  if (aiLocked()) return;
  const asked = ++boundsAsked;
  useReels.setState({ trim: { id, from, to } });
  let bounds: ReelCandidate;
  try {
    bounds = await api.reelBounds(id, from, to);
  } catch (e) {
    if (asked === boundsAsked) useReels.setState({ trim: null });
    useEditor.getState().toast({ kind: "error", text: plainError(errorText(e)) });
    return;
  }
  // A newer change is on its way and measures again.
  if (asked !== boundsAsked) return;
  const { from: f, to: t, startUs, endUs, hook, durationUs } = bounds;
  await update(id, (c) => ({ ...c, from: f, to: t, startUs, endUs, hook, durationUs }), `reel-bounds:${id}`);
  if (asked === boundsAsked) useReels.setState({ trim: null });
}

/** Why Find reels cannot ask the AI panel's agent now, or null. */
export function findBlocked(): string | null {
  const { agents } = useAgent.getState();
  if (agents && !agents.some((a) => a.path)) return "Install Claude Code or Codex first";
  return sendBlocked();
}

/** Asks the AI panel's agent for reels; they show up in the list as it proposes them. */
export function findReels() {
  if (aiLocked() || findBlocked()) return;
  useDock.setState({ open: true });
  useReels.setState({ finding: true });
  void send(FIND_REELS, "project");
}

// Finding ends with the agent's answer; an empty list from then on means nothing was found.
useAgent.subscribe((s, prev) => {
  if (prev.status !== "idle" && s.status === "idle" && useReels.getState().finding) useReels.setState({ finding: false, searched: true });
});

export function setFraming(framing: ReelFraming) {
  localStorage.setItem(FRAMING_KEY, framing);
  useReels.setState({ framing });
}

/** Makes a project of each checked candidate as a job with progress and Stop. */
export async function makeReels() {
  const { snap } = useEditor.getState();
  const { words } = useReels.getState().transcript;
  const ids = (snap?.project.reelCandidates ?? []).filter((c) => c.status === "picked" && !moved(c, words)).map((c) => c.id);
  if (ids.length === 0 || aiLocked() || useReels.getState().job) return;
  // Picks made just before are part of what is made.
  await whenIdle();
  useReels.setState({ failed: null });
  try {
    const id = await api.startMakeReels(ids, useReels.getState().framing, currentEpoch());
    useReels.setState({ job: id });
    // A job can end before its id comes back; its last event is already in.
    const ended = useEditor.getState().jobs[id];
    if (ended) onReelsJob(ended);
  } catch (e) {
    useReels.setState({ failed: makeError(errorText(e)) });
  }
}

/** What went wrong in words that say what to do next. */
function makeError(text: string): string {
  if (text.includes("SPEECH_CHANGED")) return "The video changed since these reels were found. Use Find again.";
  if (text.includes("RUN_ACTIVE")) return "The AI started editing, so no reels were made. Try again when it's done.";
  if (text.includes("os error")) return "Couldn't save the reels beside the long video. Free some space, or check that its folder can be written to.";
  return `Couldn't make the reels: ${plainError(text)}`;
}

export function onReelsJob(job: JobEvent) {
  if (job.id !== useReels.getState().job || job.status === "running") return;
  useReels.setState({ job: null, failed: job.status === "failed" ? makeError(job.message ?? "") : null });
  if (job.status !== "done") return;
  const made = JSON.parse(job.output ?? "[]") as MadeReel[];
  useEditor.getState().toast({ kind: "success", text: made.length === 1 ? "Made 1 reel" : `Made ${made.length} reels` });
}

/**
 * Opens a made reel's project, with the Export dialog on Reels & TikTok when `exporting`. The mode closes once it is
 * open; a toast leads back to the long video's reels.
 */
export function openReel(c: ReelCandidate, exporting: boolean) {
  const source = useEditor.getState().snap;
  if (!c.projectPath || !source) return;
  const back = { path: source.path, name: source.project.name };
  openProject({ path: c.projectPath, name: c.title }, undefined, () => {
    if (exporting) useEditor.setState({ exportOpen: true, exportPreset: "reels" });
    useEditor.getState().toast({ kind: "info", text: `Opened the reel “${c.title}”`, action: { label: "Back to reels", run: () => openProject(back, undefined, openReels) } });
  });
}

/** Opens the mode on a project just made of a long video, with the keyboard on Find reels. */
export function openReelsToFind() {
  openReels();
  window.setTimeout(() => document.querySelector<HTMLElement>("[data-find-reels]")?.focus(), 0);
}

/** Long enough to look for reels in, or already holding some. */
export const offersReels = (project: Project) => projectDuration(project) >= LONG_VIDEO_US || (project.reelCandidates?.length ?? 0) > 0;
