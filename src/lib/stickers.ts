import { create } from "zustand";
import { api, errorText } from "./api";
import { aiLocked, enqueue, useEditor } from "./store";
import type { StickerCatalog, StickerItem } from "./types";

export type TextTab = "text" | "stickers" | "emoji";

/** 0 is the yellow default; 1–5 light to dark, the order of `StickerItem.tones`. */
export type SkinTone = 0 | 1 | 2 | 3 | 4 | 5;

const TONE_KEY = "nuzky.skinTone";
const tone = (n: number) => (Number.isInteger(n) && n >= 0 && n <= 5 ? n : 0) as SkinTone;

type State = {
  tab: TextTab;
  catalog: StickerCatalog | null;
  /** Pictures as PNG data URLs, by `thumbKey`; "" for one that could not be drawn. */
  thumbs: Record<string, string>;
  tone: SkinTone;
  query: string;
  /** Keys found for `query`, best first; null while there is no query. While a new search runs they belong to an older one. */
  results: { query: string; keys: string[]; failed?: boolean } | null;
  adding: string[];
};

export const useStickers = create<State>(() => ({
  tab: "text",
  catalog: null,
  thumbs: {},
  tone: tone(Number(localStorage.getItem(TONE_KEY))),
  query: "",
  results: null,
  adding: [],
}));

/** The key that `item` shows with the chosen skin tone. */
export const toned = (item: StickerItem, tone: SkinTone) => (tone && item.tones ? item.tones[tone - 1] : item.key);

export function setTone(tone: SkinTone) {
  localStorage.setItem(TONE_KEY, String(tone));
  useStickers.setState({ tone });
}

export async function loadCatalog() {
  if (useStickers.getState().catalog) return;
  try {
    useStickers.setState({ catalog: await api.stickerCatalog() });
  } catch (e) {
    useEditor.getState().toast({ kind: "error", text: `Could not load the stickers: ${errorText(e)}` });
  }
}

const pending = new Set<string>();

/** Where `thumbs` keeps the picture of `key` drawn `px` wide. */
export const thumbKey = (key: string, px: number) => `${key}@${px}`;

/**
 * Draws the pictures of `keys` that are not drawn or on their way yet, a few rows at a time, so the first
 * ones show before a long group is done.
 */
export async function loadThumbs(keys: string[], px: number) {
  const { thumbs } = useStickers.getState();
  const missing = keys.filter((k) => !(thumbKey(k, px) in thumbs) && !pending.has(thumbKey(k, px)));
  missing.forEach((k) => pending.add(thumbKey(k, px)));
  for (let i = 0; i < missing.length; i += 40) {
    const chunk = missing.slice(i, i + 40);
    try {
      const urls = await api.stickerThumbs(chunk, px);
      useStickers.setState((s) => ({ thumbs: { ...s.thumbs, ...Object.fromEntries(chunk.map((k, j) => [thumbKey(k, px), urls[j]])) } }));
    } catch {
      // The tiles keep their empty place; the next look at them asks again.
    } finally {
      chunk.forEach((k) => pending.delete(thumbKey(k, px)));
    }
  }
}

let searched = 0;

export async function searchStickers(query: string) {
  useStickers.setState({ query });
  const call = ++searched;
  if (!query.trim()) return useStickers.setState({ results: null });
  try {
    const keys = await api.stickerSearch(query);
    if (call === searched) useStickers.setState({ results: { query, keys } });
  } catch {
    if (call === searched) useStickers.setState({ results: { query, keys: [], failed: true } });
  }
}

/**
 * Puts the sticker or emoji at the playhead on a track of its own, centred, and selects it. It waits its turn
 * behind the edits before it, and Undo waits for it, so Undo never overtakes a sticker still being drawn.
 */
export async function addSticker(key: string) {
  if (aiLocked() || useStickers.getState().adding.includes(key)) return;
  const { timeUs } = useEditor.getState();
  useStickers.setState((s) => ({ adding: [...s.adding, key] }));
  try {
    await enqueue((epoch) => api.stickerAdd(key, timeUs, epoch));
  } finally {
    useStickers.setState((s) => ({ adding: s.adding.filter((k) => k !== key) }));
  }
}
