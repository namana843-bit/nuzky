import { Car, Coffee, Flag, Lightbulb, PartyPopper, PawPrint, Shapes, Smile, User, type LucideIcon } from "lucide-react";
import { memo, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { addSticker, loadCatalog, loadThumbs, searchStickers, setTone, thumbKey, toned, useStickers, type SkinTone } from "../../lib/stickers";
import type { StickerGroup, StickerItem } from "../../lib/types";
import { Menu, SearchField, gridKeys, lockedProps, useLockReason } from "../ui";

/** Cells are 28 px pictures; twice that keeps them sharp on a HiDPI screen. */
const EMOJI_PX = 64;

const GROUP_ICONS: Record<string, LucideIcon> = {
  "Smileys and emotions": Smile,
  People: User,
  "Animals and nature": PawPrint,
  "Food and drink": Coffee,
  "Travel and places": Car,
  "Activities and events": PartyPopper,
  Objects: Lightbulb,
  Symbols: Shapes,
  Flags: Flag,
};

/** Noto's own skin colours, so the swatch matches the emoji it picks. */
const TONES: { name: string; color: string }[] = [
  { name: "Default", color: "#fcc21b" },
  { name: "Light", color: "#fadcbc" },
  { name: "Medium-light", color: "#e0bb95" },
  { name: "Medium", color: "#bf8f68" },
  { name: "Medium-dark", color: "#9b643d" },
  { name: "Dark", color: "#70534a" },
];

const Swatch = ({ tone }: { tone: SkinTone }) => (
  <span className="h-3.5 w-3.5 rounded-full shadow-[inset_0_0_0_1px_rgb(255_255_255/.18)]" style={{ background: TONES[tone].color }} />
);

/** The group at the top of the list. */
const groupInView = (scroller: HTMLElement) =>
  Math.max(0, [...scroller.querySelectorAll<HTMLElement>("[data-group]")].findLastIndex((s) => s.offsetTop <= scroller.scrollTop + 8));

const EmojiCell = memo(function EmojiCell({ item, first }: { item: StickerItem; first: boolean }) {
  const tone = useStickers((s) => s.tone);
  const key = toned(item, tone);
  const url = useStickers((s) => s.thumbs[thumbKey(key, EMOJI_PX)]);
  const adding = useStickers((s) => s.adding.includes(key));
  const lock = useLockReason();
  return (
    <button
      type="button"
      data-emoji={key}
      tabIndex={first ? 0 : -1}
      aria-label={`Add ${item.name}`}
      title={`Add ${item.name} at the playhead`}
      onClick={() => addSticker(key)}
      {...lockedProps(lock)}
      aria-busy={adding}
      className="flex aspect-square items-center justify-center rounded-lg transition-colors duration-[120ms] ease-out hover:bg-white/[.08] aria-busy:cursor-progress aria-busy:opacity-60 aria-disabled:cursor-not-allowed aria-disabled:opacity-40 aria-disabled:hover:bg-transparent"
    >
      {url ? <img src={url} alt="" draggable={false} className="h-7 w-7" /> : <span className="h-6 w-6 rounded-full bg-white/[.04]" />}
    </button>
  );
});

/** A group's pictures are drawn once it comes near the visible part of the list. */
function Section({ group, root, first }: { group: StickerGroup; root: HTMLElement | null; first: boolean }) {
  const ref = useRef<HTMLElement>(null);
  const tone = useStickers((s) => s.tone);
  useEffect(() => {
    if (!ref.current || !root) return;
    const keys = group.items.map((i) => toned(i, tone));
    const seen = new IntersectionObserver(([e]) => e.isIntersecting && loadThumbs(keys, EMOJI_PX), { root, rootMargin: "400px 0px" });
    seen.observe(ref.current);
    return () => seen.disconnect();
  }, [group, root, tone]);
  return (
    <section ref={ref} data-group={group.name} aria-label={group.name}>
      <h3 className="px-1 pt-1 pb-1.5 text-[13px] font-semibold text-fg">
        {group.name} <span className="tabular font-normal text-muted">{group.items.length}</span>
      </h3>
      <div className="mb-2.5 grid grid-cols-[repeat(auto-fill,minmax(40px,1fr))] gap-0.5">
        {group.items.map((item, i) => (
          <EmojiCell key={item.key} item={item} first={first && i === 0} />
        ))}
      </div>
    </section>
  );
}

function Results({ items }: { items: StickerItem[] }) {
  const tone = useStickers((s) => s.tone);
  const query = useStickers((s) => s.query);
  const failed = useStickers((s) => s.results?.failed);
  useEffect(() => {
    loadThumbs(
      items.map((i) => toned(i, tone)),
      EMOJI_PX,
    );
  }, [items, tone]);
  if (items.length === 0)
    return <p className="px-1 pt-6 text-center text-[12px] text-muted">{failed ? "Could not search. Try again." : `No emoji match “${query.trim()}”.`}</p>;
  return (
    <div className="grid grid-cols-[repeat(auto-fill,minmax(40px,1fr))] gap-0.5">
      {items.map((item, i) => (
        <EmojiCell key={item.key} item={item} first={i === 0} />
      ))}
    </div>
  );
}

function ToneButton() {
  const tone = useStickers((s) => s.tone);
  const [menu, setMenu] = useState<{ at: { x: number; y: number; align: "end" }; keyboard: boolean; back: HTMLElement } | null>(null);
  return (
    <>
      {/* Not an IconButton: choosing a skin tone changes nothing in the project, so it works while the AI edits. */}
      <button
        type="button"
        aria-label={`Skin tone: ${TONES[tone].name}`}
        title="Skin tone"
        aria-haspopup="menu"
        aria-expanded={!!menu}
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setMenu({ at: { x: r.right, y: r.bottom + 6, align: "end" }, keyboard: e.detail === 0, back: e.currentTarget });
        }}
        className="icon-btn shrink-0 transition-colors duration-[120ms] ease-out hover:bg-white/[.08] active:brightness-90"
      >
        <Swatch tone={tone} />
      </button>
      {menu && (
        <Menu
          items={TONES.map((t, i) => ({ label: t.name, icon: <Swatch tone={i as SkinTone} />, checked: i === tone, run: () => setTone(i as SkinTone) }))}
          at={menu.at}
          label="Skin tone"
          keyboard={menu.keyboard}
          minWidth={180}
          onClose={() => {
            menu.back.focus();
            setMenu(null);
          }}
        />
      )}
    </>
  );
}

export function EmojiPicker() {
  const catalog = useStickers((s) => s.catalog);
  const query = useStickers((s) => s.query);
  const results = useStickers((s) => s.results);
  const [scroller, setScroller] = useState<HTMLDivElement | null>(null);
  const [active, setActive] = useState(0);
  useEffect(() => {
    loadCatalog();
  }, []);

  const groups = useMemo(() => catalog?.groups.filter((g) => g.items.some((i) => i.emoji !== null)) ?? [], [catalog]);
  const byKey = useMemo(() => new Map(groups.flatMap((g) => g.items.map((i) => [i.key, i] as const))), [groups]);
  // Search also finds Nuzky's own stickers; this tab shows only emoji.
  const found = useMemo(() => (results?.keys ?? []).flatMap((k) => byKey.get(k) ?? []), [results, byKey]);

  // The buttons show at once; the groups they jump to come with the catalogue.
  const names = groups.length > 0 ? groups.map((g) => g.name) : Object.keys(GROUP_ICONS);
  const searching = !!query.trim();

  const jump = (index: number) => {
    const section = scroller?.querySelector<HTMLElement>(`[data-group="${CSS.escape(names[index])}"]`);
    if (scroller && section) scroller.scrollTo({ top: section.offsetTop });
    setActive(index);
  };
  const onScroll = () => scroller && setActive(groupInView(scroller));
  // Results start at the top; back from them, the group buttons show the group in view again.
  useEffect(() => {
    if (!scroller) return;
    if (searching) scroller.scrollTop = 0;
    else setActive(groupInView(scroller));
  }, [searching, scroller]);
  const focusFirst = () => scroller?.querySelector<HTMLElement>("[data-emoji]")?.focus();
  const groupKeys = (e: KeyboardEvent<HTMLDivElement>) => {
    const to = { ArrowLeft: active - 1, ArrowRight: active + 1, Home: 0, End: names.length - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    e.stopPropagation();
    const next = (to + names.length) % names.length;
    jump(next);
    e.currentTarget.querySelectorAll<HTMLElement>("button")[next]?.focus();
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-1.5 px-3.5 pt-1 pb-2">
        <SearchField
          value={query}
          onChange={searchStickers}
          placeholder="Search emoji"
          // Until the search for what the field says answers, the results are an older search's.
          onEnter={() => results?.query === query && found[0] && addSticker(toned(found[0], useStickers.getState().tone))}
          onDown={focusFirst}
        />
        <ToneButton />
      </div>
      {!searching && (
        <div role="toolbar" aria-label="Emoji groups" onKeyDown={groupKeys} className="flex shrink-0 justify-between px-3.5 pb-1.5">
          {names.map((name, i) => {
            const Icon = GROUP_ICONS[name] ?? Smile;
            return (
              <button
                key={name}
                type="button"
                aria-label={name}
                title={name}
                aria-current={i === active}
                tabIndex={i === active ? 0 : -1}
                onClick={() => jump(i)}
                className={`flex h-7 w-7 items-center justify-center rounded-[7px] transition-colors duration-[120ms] ease-out ${i === active ? "bg-white/[.08] text-accent" : "text-muted hover:bg-white/[.08] hover:text-fg"}`}
              >
                <Icon size={15} />
              </button>
            );
          })}
        </div>
      )}
      {/* The scrollbar's gutter stays reserved, so the grid keeps its columns between browsing and searching. */}
      <div ref={setScroller} onScroll={onScroll} onKeyDown={gridKeys("[data-emoji]")} className="relative min-h-0 flex-1 overflow-y-scroll pt-1 pr-0 pb-3.5 pl-2.5">
        {searching ? results && <Results items={found} /> : groups.map((g, i) => <Section key={g.name} group={g} root={scroller} first={i === 0} />)}
      </div>
    </div>
  );
}
