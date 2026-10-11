//! Match loudness and Even out volume. Both are gains that follow a file's sound: they come from its
//! loudness in 10 ms blocks, measured once in the background into a small file beside the cache it
//! describes (`ensure_blocks`), and the mixer applies them by the position in the file, so every piece of
//! a take, the preview and the export hear the same gain at the same sound.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::audio::{Pcm, lock_cache};
use crate::loudness::{HIGH_PASS, SHELF, gated, lufs};
use crate::model::{CHANNELS, SAMPLE_RATE};

/// Raised whenever the measurement changes, so blocks measured before are measured again.
pub const VERSION: &str = "lvl1";
/// Where Match loudness puts a file: speech at -16 LUFS leaves the export presets (-14 LUFS) a small
/// step up through their limiter and room for music under it, and peaks of phone speech under 0 dBFS.
pub const MATCH_LUFS: f64 = -16.0;
/// The most Match loudness raises or lowers a file, so near silence never turns into loud noise.
const MATCH_MAX_DB: f64 = 20.0;
pub const BLOCK: usize = SAMPLE_RATE as usize / 100;
/// BS.1770 gating blocks of 400 ms every 100 ms, in blocks.
const GATE_BLOCK: usize = 40;
const GATE_STEP: usize = 10;
/// Even out volume compares the loudness of the speech within 1.5 s either side of a block with the
/// file's. A block is speech when the 400 ms around it are at most 20 dB under the file's loudness.
const PASSAGE: usize = 100;
const SPEECH_BELOW_FILE: f64 = 20.0;
/// How far Even out volume at full strength moves a passage, and the second either side its gain is
/// averaged over, so it follows passages and never single words.
const LEVEL_MAX_DB: f64 = 12.0;
const SMOOTH: usize = 40;
/// No gain here raises a block's peak over -1 dBFS. Where one would, the gain dips 3 dB per block
/// before the peak and comes back 0.5 dB per block after it, as a limiter does.
const CEILING_DB: f64 = -1.0;
const ATTACK_DB: f64 = 3.0;
const RELEASE_DB: f64 = 0.5;

/// One block of a file's sound: its K-weighted power summed over the channels (BS.1770 z) and its
/// largest sample.
pub type Block = [f32; 2];

/// Where the blocks of the cache at `cache` are measured to.
pub fn blocks_path(cache: &Path) -> PathBuf {
    cache.with_extension(format!("{VERSION}.blocks"))
}

/// Measures the blocks of the cache at `cache` unless that is done already. An error from `progress`
/// stops the work.
pub fn ensure_blocks(cache: &Path, mut progress: impl FnMut(f32) -> Result<()>) -> Result<PathBuf> {
    let path = blocks_path(cache);
    if path.exists() {
        return Ok(path);
    }
    let _lock = lock_cache(&path, &mut progress)?;
    if !path.exists() {
        let pcm = Pcm::open(cache).with_context(|| format!("Reading {}", cache.display()))?;
        let blocks = measure(pcm.samples(), progress)?;
        let tmp = path.with_file_name(format!(".nuzky-blocks-{}.part", crate::edit::new_id()));
        std::fs::write(&tmp, bytemuck::cast_slice(&blocks))?;
        std::fs::rename(&tmp, &path).inspect_err(|_| {
            std::fs::remove_file(&tmp).ok();
        })?;
    }
    Ok(path)
}

/// The measured blocks of the cache at `cache`, or `None` while they are not there.
pub fn read_blocks(cache: &Path) -> Option<Vec<Block>> {
    let bytes = std::fs::read(blocks_path(cache)).ok()?;
    Some(bytemuck::pod_collect_to_vec::<u8, f32>(&bytes).as_chunks::<2>().0.to_vec())
}

fn measure(samples: &[f32], mut progress: impl FnMut(f32) -> Result<()>) -> Result<Vec<Block>> {
    let mut weighting = [[SHELF, HIGH_PASS]; CHANNELS];
    let frames = samples.len() / CHANNELS;
    let mut blocks = Vec::with_capacity(frames.div_ceil(BLOCK));
    for (index, block) in samples.chunks(BLOCK * CHANNELS).enumerate() {
        let (mut power, mut peak) = (0.0f64, 0f32);
        for frame in block.as_chunks::<CHANNELS>().0 {
            for ([shelf, high_pass], &x) in weighting.iter_mut().zip(frame) {
                let x = if x.is_finite() { x } else { 0.0 };
                let y = high_pass.run(shelf.run(x as f64));
                power += y * y;
                peak = peak.max(x.abs());
            }
        }
        blocks.push([(power / BLOCK as f64) as f32, peak]);
        if index % 100 == 99 {
            progress(index as f32 / frames.div_ceil(BLOCK) as f32)?;
        }
    }
    Ok(blocks)
}

/// The blocks of `share` of `wet` mixed into `dry`, two versions of one sound in step with each other,
/// as Reduce echo plays them. The level adds as for the same sound, the peak is at most the mixed peaks.
pub fn blend(dry: &[Block], wet: &[Block], share: f32) -> Vec<Block> {
    let share = share.clamp(0.0, 1.0);
    dry.iter()
        .zip(wet)
        .map(|(d, w)| {
            let amplitude = (1.0 - share) * d[0].sqrt() + share * w[0].sqrt();
            [amplitude * amplitude, (1.0 - share) * d[1] + share * w[1]]
        })
        .collect()
}

/// What Match loudness and Even out volume do to a file.
pub struct Levels {
    /// The gain of Match loudness in dB; `None` without it, or for a file with nothing above -70 LUFS.
    pub match_db: Option<f64>,
    /// The gain of every block, linear.
    pub gains: Vec<f32>,
}

/// Match loudness when `normalize` and Even out volume by `leveler` (0 to 1) for a file of `blocks`.
pub fn levels(blocks: &[Block], normalize: bool, leveler: f32) -> Levels {
    let n = blocks.len();
    let power = |b: usize| blocks[b][0] as f64;
    let file = integrated(n, power);
    let mut db = vec![0.0f64; n];
    if let Some(file) = file.filter(|_| leveler > 0.0) {
        // Speech within reach of each block, by running sums.
        let local = sums(n, power);
        let speech: Vec<bool> = (0..n)
            .map(|b| {
                let (from, to) = (b.saturating_sub(GATE_BLOCK / 2), (b + GATE_BLOCK / 2).min(n));
                lufs((local[to] - local[from]) / (to - from) as f64) > (file - SPEECH_BELOW_FILE).max(-70.0)
            })
            .collect();
        let spoken = sums(n, |b| if speech[b] { power(b) } else { 0.0 });
        let count = sums(n, |b| speech[b] as u8 as f64);
        let mut gap: Vec<Option<f64>> = (0..n)
            .map(|b| {
                let (from, to) = (b.saturating_sub(PASSAGE), (b + PASSAGE + 1).min(n));
                let speech = count[to] - count[from];
                (speech > 0.0)
                    .then(|| (file - lufs((spoken[to] - spoken[from]) / speech)).clamp(-LEVEL_MAX_DB, LEVEL_MAX_DB))
            })
            .collect();
        // Pauses longer than a passage keep the gain of the speech next to them.
        for b in 1..n {
            gap[b] = gap[b].or(gap[b - 1]);
        }
        for b in (0..n.saturating_sub(1)).rev() {
            gap[b] = gap[b].or(gap[b + 1]);
        }
        let gap = sums(n, |b| gap[b].unwrap_or(0.0));
        for (b, db) in db.iter_mut().enumerate() {
            let (from, to) = (b.saturating_sub(SMOOTH), (b + SMOOTH + 1).min(n));
            *db = leveler.min(1.0) as f64 * (gap[to] - gap[from]) / (to - from) as f64;
        }
    }
    // Measured after evening out, so the two together land on the target.
    let match_db = normalize
        .then(|| integrated(n, |b| power(b) * 10f64.powf(db[b] / 10.0)))
        .flatten()
        .map(|lufs| (MATCH_LUFS - lufs).clamp(-MATCH_MAX_DB, MATCH_MAX_DB));
    for db in &mut db {
        *db += match_db.unwrap_or(0.0);
    }
    // A gain under 0 dB never needs to come down further for a peak.
    let room: Vec<f64> = blocks
        .iter()
        .map(|b| if b[1] > 0.0 { (CEILING_DB - 20.0 * (b[1] as f64).log10()).max(0.0) } else { f64::INFINITY })
        .collect();
    let mut capped: Vec<f64> = (0..n)
        .map(|b| db[b].min(room[b]).min(room[b.saturating_sub(1)]).min(room[(b + 1).min(n.saturating_sub(1))]))
        .collect();
    for b in 1..n {
        capped[b] = capped[b].min(capped[b - 1] + RELEASE_DB);
    }
    for b in (0..n.saturating_sub(1)).rev() {
        capped[b] = capped[b].min(capped[b + 1] + ATTACK_DB);
    }
    Levels { match_db, gains: capped.iter().map(|&db| crate::loudness::db_to_gain(db)).collect() }
}

/// The gain at `frame` of the file, from the gains of its blocks, linear between block middles.
pub fn gain_at(gains: &[f32], frame: f64) -> f32 {
    if gains.is_empty() {
        return 1.0;
    }
    let at = (frame / BLOCK as f64 - 0.5).clamp(0.0, gains.len().saturating_sub(1) as f64);
    let (index, t) = (at as usize, (at - at.floor()) as f32);
    let (a, b) = (gains[index], gains[(index + 1).min(gains.len() - 1)]);
    a + (b - a) * t
}

/// Integrated loudness (BS.1770) of `n` blocks of `power`, or `None` with nothing above -70 LUFS.
fn integrated(n: usize, power: impl Fn(usize) -> f64) -> Option<f64> {
    let gating: Vec<f64> = (0..=n.saturating_sub(GATE_BLOCK))
        .step_by(GATE_STEP)
        .filter(|&b| b + GATE_BLOCK <= n)
        .map(|b| (b..b + GATE_BLOCK).map(&power).sum::<f64>() / GATE_BLOCK as f64)
        .collect();
    gated(&gating)
}

/// Running sums: element `b` is the sum of `value` over blocks before `b`.
fn sums(n: usize, value: impl Fn(usize) -> f64) -> Vec<f64> {
    let mut out = Vec::with_capacity(n + 1);
    out.push(0.0);
    for b in 0..n {
        out.push(out[b] + value(b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocks of a sound whose 10 ms blocks have the loudness `lufs(b)` and the peak `peak(b)` dBFS.
    fn blocks(n: usize, lufs: impl Fn(usize) -> f64, peak: impl Fn(usize) -> f64) -> Vec<Block> {
        (0..n).map(|b| [10f64.powf((lufs(b) + 0.691) / 10.0) as f32, 10f64.powf(peak(b) / 20.0) as f32]).collect()
    }

    fn db(gain: f32) -> f64 {
        20.0 * (gain as f64).log10()
    }

    /// Loudness of `blocks` played at `gains`.
    fn played(blocks: &[Block], gains: &[f32]) -> f64 {
        integrated(blocks.len(), |b| blocks[b][0] as f64 * (gains[b] as f64).powi(2)).unwrap()
    }

    #[test]
    fn match_loudness_lands_on_the_target_and_holds_peaks_under_the_ceiling() {
        // A quiet take at -30 LUFS with a clap at -3 dBFS at 5 s.
        let take = blocks(1_000, |_| -30.0, |b| if b == 500 { -3.0 } else { -18.0 });
        let Levels { match_db, gains } = levels(&take, true, 0.0);
        assert!((match_db.unwrap() - 14.0).abs() < 0.01, "{match_db:?}");
        assert!((played(&take, &gains) - MATCH_LUFS).abs() < 0.1);
        // Raised by 14 dB, the clap would reach +11 dBFS; it stays at the ceiling. The gain dips 40 ms before
        // it and is back 250 ms after it.
        let clap = (499..=501).map(|b| db(gains[b]) + 20.0 * (take[b][1] as f64).log10()).fold(f64::MIN, f64::max);
        assert!(clap <= CEILING_DB + 1e-6, "the clap peaks at {clap:.2} dBFS");
        assert!((db(gains[526]) - 14.0).abs() < 0.01 && (db(gains[495]) - 14.0).abs() < 0.01);
        // Silence gets nothing, and a loud take comes down.
        assert_eq!(levels(&blocks(1_000, |_| -90.0, |_| -80.0), true, 0.0).match_db, None);
        assert!((levels(&blocks(1_000, |_| -8.0, |_| -1.0), true, 0.0).match_db.unwrap() + 8.0).abs() < 0.01);
    }

    #[test]
    fn even_out_volume_brings_passages_together_and_keeps_the_pauses_at_their_neighbours() {
        // 4 s passages at -20 and -32 LUFS, with 0.5 s pauses of room tone between them.
        let passage = |b: usize| match b % 450 {
            0..400 if (b / 450).is_multiple_of(2) => -20.0,
            0..400 => -32.0,
            _ => -60.0,
        };
        let take = blocks(450 * 8, passage, |b| passage(b) + 12.0);
        let spread = |gains: &[f32]| {
            let level = |odd: usize| {
                let middles = (0..8).filter(|k| k % 2 == odd).map(|k| k * 450 + 200);
                middles.map(|b| -passage(b) - db(gains[b])).sum::<f64>() / 4.0
            };
            level(1) - level(0)
        };
        let none = levels(&take, false, 0.0).gains;
        let half = levels(&take, false, 0.5).gains;
        let full = levels(&take, false, 1.0).gains;
        eprintln!("passages 12 dB apart: {:.1} dB at 0.5, {:.1} dB at 1", spread(&half), spread(&full));
        assert!((spread(&none) - 12.0).abs() < 0.01);
        assert!((spread(&half) - 6.0).abs() < 1.5 && spread(&full) < 3.0);
        // The gain moves slowly: at most a few tenths of a dB per 10 ms, so no pumping.
        let steepest = full.windows(2).map(|w| (db(w[1]) - db(w[0])).abs()).fold(0.0, f64::max);
        assert!(steepest < 0.3, "{steepest:.2} dB in one block");
    }

    #[test]
    fn the_gain_between_blocks_is_linear_and_continuous() {
        let gains = [1.0, 2.0, 4.0];
        assert_eq!(gain_at(&gains, 0.0), 1.0);
        assert_eq!(gain_at(&gains, BLOCK as f64 * 1.5), 2.0);
        assert_eq!(gain_at(&gains, BLOCK as f64 * 2.0), 3.0);
        assert_eq!(gain_at(&gains, 1e9), 4.0);
        assert_eq!(gain_at(&[], 10.0), 1.0);
    }
}
