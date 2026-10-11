import { Pause, Play, SkipBack, SkipForward } from "lucide-react";
import { candidateOf, closeReels, playEnding, toggleReelPlay, useReels } from "../../lib/reels";
import { useEditor } from "../../lib/store";
import { formatTime } from "../../lib/time";
import { Button, IconButton } from "../ui";

/** The playhead in the recording; the reel's span stands over its words. */
function Where() {
  const timeUs = useEditor((s) => s.timeUs);
  return <span className="tabular truncate text-[13px] font-medium text-fg">{formatTime(timeUs)}</span>;
}

/** The transport while choosing reels: the reel from its start, its ending, and Done. */
export function ReelBar() {
  const playing = useEditor((s) => s.playing);
  const current = useReels((s) => s.current);
  const c = useEditor((s) => candidateOf(s.snap?.project, current));
  const none = !c;
  return (
    <div className="bar grid h-11 w-full max-w-[620px] grid-cols-[1fr_auto_1fr] items-center gap-2 rounded-full pl-[18px] pr-1.5">
      <Where />
      <div className="flex items-center gap-2.5">
        <IconButton round label="Reel start (Home)" disabled={none} onClick={() => c && useEditor.getState().seek(c.startUs)}>
          <SkipBack size={16} />
        </IconButton>
        <button
          type="button"
          aria-label={playing ? "Pause (Space)" : "Play the reel (Space)"}
          title={playing ? "Pause (Space)" : "Play the reel (Space)"}
          disabled={none}
          onClick={toggleReelPlay}
          className="flex h-9 w-9 items-center justify-center rounded-full bg-fg text-black shadow-[0_1px_3px_rgb(0_0_0/.4)] transition-transform duration-[120ms] ease-out active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
        >
          {playing ? <Pause size={16} fill="currentColor" /> : <Play size={16} fill="currentColor" className="translate-x-px" />}
        </button>
        <IconButton round label="Hear the ending (End)" disabled={none} onClick={playEnding}>
          <SkipForward size={16} />
        </IconButton>
      </div>
      <div className="flex justify-end">
        <Button pill className="h-8 px-3.5" title="Back to editing (Esc)" onClick={closeReels}>
          Done
        </Button>
      </div>
    </div>
  );
}

/** What Center crop keeps of a wider frame: the middle 9:16, the sides dimmed as the safe zone dims. */
export function CropGuide({ width, height }: { width: number; height: number }) {
  const kept = (height * 9) / 16 / width;
  if (kept >= 1) return null;
  const side = `${((1 - kept) / 2) * 100}%`;
  return (
    <>
      <div className="pointer-events-none absolute inset-y-0 left-0 bg-black/45" style={{ width: side }} />
      <div className="pointer-events-none absolute inset-y-0 right-0 bg-black/45" style={{ width: side }} />
      <div className="pointer-events-none absolute inset-y-0 border-x border-dashed border-fg/50" style={{ left: side, right: side }} />
    </>
  );
}
