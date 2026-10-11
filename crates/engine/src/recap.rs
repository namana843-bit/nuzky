//! A recap: a short vertical video of how a video was cut. The recording plays fast with what was cut
//! marked, the transcript strikes the cut words, a counter falls from the length before to the length
//! after, and a sample of the finished cut closes it. Its pictures come from the renderer of preview
//! and export, so the recording and the cut look as they do in the app.

use std::path::Path;
use std::sync::atomic::AtomicBool;

use anyhow::{Result, bail};

use crate::model::{Asset, Project};

/// What a recap shows. `cuts` and `words` are on the timeline of `before`.
#[derive(Clone, Debug)]
pub struct Recap {
    pub before: Project,
    pub after: Project,
    /// Time of `before` that `after` no longer plays, in order, apart and not touching.
    pub cuts: Vec<RecapCut>,
    /// The transcript of `before`, in order; empty when `transcribed` is false.
    pub words: Vec<RecapWord>,
    /// Every file a cut touches has a transcript, so words, retakes and pauses are known.
    pub transcribed: bool,
    /// Built-in sounds the recap plays: a tick as each cut passes and a whoosh between its parts.
    pub tick: Option<Asset>,
    pub whoosh: Option<Asset>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecapCut {
    pub start_us: i64,
    pub end_us: i64,
    /// Transcript words whose middle falls in the cut.
    pub words: usize,
    /// Retakes the cut removed: attempts at a sentence said again later, and whole takes.
    pub retakes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecapWord {
    pub start_us: i64,
    pub end_us: i64,
    pub text: String,
    /// Its middle falls in a cut.
    pub removed: bool,
}

/// What a recap counts, the same numbers the run summary gives.
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecapTotals {
    pub before_us: i64,
    pub after_us: i64,
    /// `None` while a file a cut touches has no transcript.
    pub words: Option<usize>,
    pub retakes: Option<usize>,
    /// Cuts without a word.
    pub pauses: Option<usize>,
}

impl Recap {
    pub fn totals(&self) -> RecapTotals {
        let known = |n: usize| self.transcribed.then_some(n);
        RecapTotals {
            before_us: self.before.duration_us(),
            after_us: self.after.duration_us(),
            words: known(self.cuts.iter().map(|c| c.words).sum()),
            retakes: known(self.cuts.iter().map(|c| c.retakes).sum()),
            pauses: known(self.cuts.iter().filter(|c| c.words == 0).count()),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RecapOptions {
    /// A short card with nuzky.app at the end.
    pub end_card: bool,
    pub replace_existing: bool,
}

const HOOK_US: i64 = 1_500_000;
/// The recording plays at least this fast and takes at most this long.
const FAST_MIN_US: i64 = 3_000_000;
const FAST_MAX_US: i64 = 17_000_000;
const HOLD_US: i64 = 600_000;
const SAMPLE_MAX_US: i64 = 8_000_000;
const END_CARD_US: i64 = 1_500_000;

/// The parts of a recap, as lengths: the opening numbers, the recording played fast, the counter held,
/// the finished cut and the end card (0 when off).
pub fn sections(recap: &Recap, end_card: bool) -> [i64; 5] {
    let before = recap.before.duration_us();
    let fast = (before / 4).clamp(FAST_MIN_US, FAST_MAX_US).min(before);
    let sample = recap.after.duration_us().min(SAMPLE_MAX_US);
    [HOOK_US, fast, HOLD_US, sample, if end_card { END_CARD_US } else { 0 }]
}

pub fn duration_us(recap: &Recap, end_card: bool) -> i64 {
    sections(recap, end_card).iter().sum()
}

/// Writes the recap to `out` through a temporary `.nuzky-part-*` beside it, as export does.
pub fn render(
    recap: &Recap,
    cache_dir: &Path,
    out: &Path,
    options: &RecapOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(f32),
) -> Result<()> {
    let _ = (recap, cache_dir, out, options, cancel, progress);
    bail!("Recap rendering is not ready yet")
}
