import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { AlertTriangle } from "lucide-react";
import { REEL_LIMIT_US, candidateOf, setBounds, toggleReelPlay, useReels } from "../../lib/reels";
import { aiLocked, useEditor } from "../../lib/store";
import { paragraphs, tokenAt, type Token } from "../../lib/speech";
import { US, formatDuration, formatTime } from "../../lib/time";
import type { Project, TranscriptWord } from "../../lib/types";
import { useLockReason } from "../ui";

/** How much is said around a reel, to trim it into. */
const CONTEXT_US = 60 * US;

/** The words a reel may grow into: up to a minute each side, never into a reel not made yet. */
function reach(project: Project, words: TranscriptWord[], id: string, from: number, to: number): [number, number] {
  const others = (project.reelCandidates ?? []).filter((c) => c.id !== id && c.status !== "made");
  let lo = from;
  while (lo > 0 && words[from].startUs - words[lo - 1].startUs <= CONTEXT_US && !others.some((c) => c.to === lo - 1)) lo--;
  let hi = to;
  while (hi < words.length - 1 && words[hi + 1].endUs - words[to].endUs <= CONTEXT_US && !others.some((c) => c.from === hi + 1)) hi++;
  return [lo, hi];
}

/**
 * The current reel's words in the library's place, with what is said around them. A click plays from a word,
 * Shift-click moves the nearer end of the reel to it; ←/→ go word by word, Alt+←/→ move its start and Shift+←/→ its end.
 */
export function ReelTrim() {
  const project = useEditor((s) => s.snap?.project);
  const id = useReels((s) => s.current);
  const trim = useReels((s) => s.trim);
  const c = candidateOf(project, id);
  const { words, error } = useReels((s) => s.transcript);
  const lock = useLockReason();
  // The bounds asked for show at once; the engine's length follows.
  const from = trim?.id === id ? trim.from : (c?.from ?? 0);
  const to = trim?.id === id ? trim.to : (c?.to ?? 0);
  // A cut made since can move the words a reel names; then it shows none of them.
  const fits = !!c && words?.[c.from]?.startUs === c.startUs && words?.[c.to]?.endUs === c.endUs;
  const [lo, hi] = useMemo(() => (project && c && words && fits ? reach(project, words, c.id, c.from, c.to) : [0, -1]), [project, c, words, fits]);
  const tokens = useMemo<Token[]>(() => (words ? words.slice(lo, hi + 1).map((w) => ({ kind: "word", ...w })) : []), [words, lo, hi]);
  const paras = useMemo(() => paragraphs(tokens), [tokens]);
  const active = useEditor((s) => (tokens.length ? tokenAt(tokens, s.timeUs) : -1));
  const playing = useEditor((s) => s.playing);
  const [caret, setCaret] = useState<number | null>(null);
  const box = useRef<HTMLDivElement>(null);

  // Another reel starts with the caret on its first word.
  useEffect(() => setCaret(null), [id]);
  useEffect(() => {
    const at = caret ?? (playing ? active + lo : null);
    if (at !== null) box.current?.querySelector(`[data-w="${at}"]`)?.scrollIntoView({ block: "nearest" });
  }, [caret, active, playing, lo]);
  // A reel shown anew scrolls to its start, but trimming it does not.
  const start = useRef(c?.from);
  start.current = c?.from;
  useEffect(() => {
    box.current?.querySelector(`[data-w="${start.current}"]`)?.scrollIntoView({ block: "center" });
  }, [id, fits]);

  if (!c) return <p className="m-auto px-6 text-center text-[13px] text-muted">Choose a reel to trim its words.</p>;
  const made = c.status === "made";
  const long = c.durationUs > REEL_LIMIT_US;
  const seek = (us: number) => useEditor.getState().seek(us);

  const move = (f: number, t: number, show: "start" | "end") => {
    if (made || !words || (f === from && t === to)) return;
    void setBounds(c.id, f, t);
    seek(show === "start" ? words[f].startUs : Math.max(words[f].startUs, words[t].endUs - 3 * US));
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    const w = e.target instanceof Element ? e.target.closest("[data-w]") : null;
    if (e.button !== 0 || !w || !words) return;
    e.preventDefault();
    box.current?.focus();
    const i = Number(w.getAttribute("data-w"));
    setCaret(i);
    if (!e.shiftKey) return seek(words[i].startUs);
    if (made) return;
    if (aiLocked()) return;
    // The nearer end goes to the word: before the reel or in its first half its start, else its end.
    if (i < from || (i <= to && i - from < to - i)) move(i, to, "start");
    else move(from, i, "end");
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!words) return;
    const dir = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    const handled = () => (e.preventDefault(), e.stopPropagation());
    if (dir && (e.altKey || e.shiftKey)) {
      handled();
      if (made || aiLocked()) return;
      if (e.altKey) move(Math.max(lo, Math.min(to, from + dir)), to, "start");
      else move(from, Math.max(from, Math.min(hi, to + dir)), "end");
    } else if (dir || e.key === "Home" || e.key === "End") {
      handled();
      const at = e.key === "Home" ? from : e.key === "End" ? to : Math.max(lo, Math.min(hi, (caret ?? (active >= 0 ? active + lo : from)) + dir));
      setCaret(at);
      seek(words[at].startUs);
    } else if (e.key === " ") {
      handled();
      toggleReelPlay();
    }
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 flex-col gap-0.5 border-b border-white/[.07] px-3.5 py-3">
        <h2 className="truncate text-[13px] font-semibold text-fg" title={c.title}>
          {c.title}
        </h2>
        <p className={`tabular flex items-center gap-1.5 text-[12px] ${long ? "text-warn" : "text-muted"}`}>
          {long && <AlertTriangle size={12} className="shrink-0" />}
          {formatDuration(c.durationUs)} once cut{long ? ", over a minute" : ""} · {formatTime(c.startUs, false)}–{formatTime(c.endUs, false)}
        </p>
      </div>
      {error && <p className="p-3.5 text-[12px] text-danger">{error}</p>}
      {words && !fits && <p className="p-3.5 text-[12px] text-muted">The video changed since this reel was found. Find reels again.</p>}
      {fits && (
        <div
          ref={box}
          role="listbox"
          aria-label={`Words of “${c.title}”`}
          aria-activedescendant={caret !== null ? `reel-word-${caret}` : undefined}
          aria-description={made ? undefined : "Shift-click a word, or press Alt or Shift with the arrows, to move the start or the end of the reel"}
          tabIndex={0}
          onPointerDown={onPointerDown}
          onKeyDown={onKeyDown}
          className="min-h-0 flex-1 overflow-y-auto px-3.5 pt-3 pb-1 focus-visible:-outline-offset-2!"
        >
          {paras.map(([a, b]) => (
            <p key={a} className="mb-3 grid grid-cols-[40px_1fr] text-[13px] leading-[22px]">
              <span className="tabular text-[11px] text-muted">{formatTime(tokens[a].startUs, false)}</span>
              <span>
                {tokens.slice(a, b).map((t, n) => {
                  const i = lo + a + n;
                  const inside = i >= from && i <= to;
                  const now = a + n === active;
                  const look = inside ? (now ? "bg-accent text-black" : "bg-accent/30 text-fg") : now ? "text-accent" : "text-muted";
                  return (
                    <span key={i}>
                      {n > 0 && <span className={inside && i > from ? "bg-accent/30" : undefined}> </span>}
                      <span
                        id={`reel-word-${i}`}
                        data-w={i}
                        role="option"
                        aria-selected={inside}
                        className={`cursor-pointer ${inside ? `${i === from ? "rounded-l-sm" : ""} ${i === to ? "rounded-r-sm" : ""}` : "rounded-sm hover:bg-raised"} ${i === caret ? "outline-1 outline-accent" : ""} ${look}`}
                      >
                        {t.kind === "word" ? t.text : ""}
                      </span>
                    </span>
                  );
                })}
              </span>
            </p>
          ))}
        </div>
      )}
      {fits && <p className="shrink-0 border-t border-white/[.07] px-3.5 py-2 text-[12px] text-muted">{made ? "Made reels keep their words. Open it to change it." : (lock ?? "Shift-click a word to start or end the reel there.")}</p>}
    </div>
  );
}
