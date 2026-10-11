//! Reduce echo in two steps, both on a short-time Fourier transform of the sound. First weighted
//! prediction error (WPE, Nakatani et al. 2010, as the nara_wpe package (MIT) implements it): in each
//! frequency band the late reverb of a room is what can be predicted from the sound a few frames before,
//! and WPE takes that prediction away, weighting quiet frames up, where reverb tails are all there is.
//! It hardly changes the voice and works best with two microphones; a phone's mono sound keeps much of its
//! reverb. Then late reverb suppression (Lebart 2001, Habets 2009) turns each band down where its level is
//! what the room's decay leaves of the sound 50 ms before, by the reverb time measured from the file's
//! own decays. The direct sound and early reflections stay, so the voice keeps its body. No model,
//! nothing to download.
//!
//! The result is another PCM cache per file, made once in the background like Clean voice, with the
//! raw cache's length and every sound on the same sample. Clips mix it with the original by their
//! Reduce echo share.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use realfft::RealFftPlanner;
use realfft::num_complex::Complex32;

use crate::audio::{PCM_VERSION, Pcm, ensure_pcm, lock_cache, pcm_path, remove_older_versions, with_blocks};
use crate::model::{Asset, CHANNELS, SAMPLE_RATE};

/// Raised whenever the processing changes its output, so sound made before is made again.
pub const VERSION: &str = "echo1";

/// 43 ms frames every 11 ms.
const FFT: usize = 2048;
const HOP: usize = FFT / 4;
const BINS: usize = FFT / 2 + 1;
/// The prediction reads `TAPS` frames, starting `DELAY` frames back: reverb from 32 ms to 350 ms.
/// nara_wpe's 10 taps took 0.2 dB of a 0.8 s reverb tail in mono, 30 took 1.6 dB, in stereo 13 dB.
const TAPS: usize = 30;
const DELAY: usize = 3;
const HISTORY: usize = TAPS + DELAY - 1;
const ITERATIONS: usize = 3;
/// Frames whose prediction is found together, 20 s, with 1 s more on each side so the filters of
/// neighbouring blocks agree where they meet.
const BLOCK: usize = 1875;
const CONTEXT: usize = 94;
/// Bands processed between two looks at a stop request, so stopping takes a fraction of a block.
const BANDS_PER_STEP: usize = 128;
/// Floor of a frame's power relative to the loudest frame's, so silence does not divide by zero.
const POWER_FLOOR: f64 = 1e-10;
/// Added to the diagonal of each band's correlation, relative to its mean, against bands without sound.
const LOADING: f64 = 1e-9;
/// Late reverb suppression: reverb counts as late 50 ms after the sound, levels are smoothed over 40 ms
/// (shorter smoothing left less of the tails out and made room tone flutter), a band goes down by at most
/// 12 dB, and the reverb time is between 0.2 and 1.5 s, 0.5 s when the file has no clear decays.
const LATE_FRAMES: usize = 5;
const SMOOTHING_S: f64 = 0.04;
const DIRECTED: f64 = 0.9;
const FLOOR_DB: f64 = -12.0;
const T60_RANGE: (f64, f64) = (0.2, 1.5);
const T60_DEFAULT: f64 = 0.5;
/// Part of the progress bar the raw cache takes when it is extracted first.
const RAW_SHARE: f32 = 0.1;

/// Where the sound of the raw cache at `raw` is cached without its reverb.
pub fn path_for(cache_dir: &Path, raw: &Path) -> PathBuf {
    let stem = raw.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    cache_dir.join("dereverb").join(format!("{stem}.{VERSION}.f32"))
}

/// Takes the reverb out of the sound of `asset` unless that is done already, extracting its raw cache
/// first when needed. Concurrent callers wait for one of them; an error from `progress` stops the work.
pub fn ensure_dereverb_pcm(
    cache_dir: &Path,
    asset: &Asset,
    mut progress: impl FnMut(f32) -> Result<()>,
) -> Result<PathBuf> {
    let raw_share = if pcm_path(cache_dir, asset).exists() { 0.0 } else { RAW_SHARE };
    let raw = ensure_pcm(cache_dir, asset, |p| progress(p * raw_share))?;
    let path = path_for(cache_dir, &raw);
    let _lock = lock_cache(&path, &mut |_| progress(raw_share))?;
    if !path.exists() {
        process_file(&raw, &path, |p| progress(raw_share + p * (1.0 - raw_share)))?;
        remove_older_versions(&path, asset, &with_blocks(format!("{PCM_VERSION}.{VERSION}")));
    }
    Ok(path)
}

fn process_file(raw: &Path, out: &Path, progress: impl FnMut(f32) -> Result<()>) -> Result<()> {
    let pcm = Pcm::open(raw).with_context(|| format!("Reading {}", raw.display()))?;
    let tmp = out.with_file_name(format!(".nuzky-dereverb-{}.part", crate::edit::new_id()));
    let file = File::options().write(true).create_new(true).open(&tmp)?;
    let result = (|| {
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        dereverb(pcm.samples(), |frames| Ok(writer.write_all(bytemuck::cast_slice(frames))?), progress)?;
        writer.flush()?;
        drop(writer);
        std::fs::rename(&tmp, out)?;
        Ok(())
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

/// Takes the reverb out of interleaved stereo `input`, handing the result to `out` in order. The
/// output has exactly the input's length and every sound on the same sample. `progress` gets the share
/// done several times a block; an error from it stops the work.
pub fn dereverb(
    input: &[f32],
    mut out: impl FnMut(&[f32]) -> Result<()>,
    mut progress: impl FnMut(f32) -> Result<()>,
) -> Result<()> {
    let len = input.len() / CHANNELS;
    // Phone video is often mono stored as two equal channels; it is processed once.
    let mono = input.as_chunks::<CHANNELS>().0.iter().all(|f| f[0] == f[1]);
    let channels = if mono { 1 } else { CHANNELS };
    let decay = late_decay(reverb_time(input));
    let mut planner = RealFftPlanner::<f32>::new();
    let (forward, inverse) = (planner.plan_fft_forward(FFT), planner.plan_fft_inverse(FFT));
    // A periodic Blackman window, as nara_wpe's, keeps bands apart better than Hann. The synthesis
    // window divides by the squared windows overlapping each sample, so the output is the input exactly.
    let window: Vec<f32> = (0..FFT)
        .map(|n| {
            let phase = std::f64::consts::TAU * n as f64 / FFT as f64;
            (0.42 - 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos()) as f32
        })
        .collect();
    let overlapping = |n: usize| (n % HOP..FFT).step_by(HOP).map(|m| window[m] * window[m]).sum::<f32>();
    let synthesis: Vec<f32> = (0..FFT).map(|n| window[n] / overlapping(n) / FFT as f32).collect();
    // Frame `t` starts at sample `t * HOP - (FFT - HOP)`, so every sample lies in four frames.
    let start = |t: i64| t * HOP as i64 - (FFT - HOP) as i64;
    let frames = (len + FFT - HOP).div_ceil(HOP);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut buffer = forward.make_input_vec();
    let mut spectrum = forward.make_output_vec();
    // Each channel's overlap-add from the first sample not yet written.
    let mut overlap = vec![vec![0f32; FFT - HOP]; channels];
    let mut written = 0usize;
    let mut interleaved = Vec::new();
    for first in (0..frames).step_by(BLOCK) {
        let count = BLOCK.min(frames - first);
        // The frames whose filter is found together, and the frames before them a prediction reads.
        let (from, to) = (first.saturating_sub(CONTEXT), (first + count + CONTEXT).min(frames));
        let (span, found) = (to - from + HISTORY, to - from);
        // Bands in order, each its frames in order and each frame its channels.
        let mut y = vec![Complex32::ZERO; BINS * span * channels];
        for j in 0..span {
            let t = (from + j) as i64 - HISTORY as i64;
            if t < 0 {
                continue;
            }
            for c in 0..channels {
                for (n, x) in buffer.iter_mut().enumerate() {
                    let i = start(t) + n as i64;
                    *x = if (0..len as i64).contains(&i) { input[i as usize * CHANNELS + c] * window[n] } else { 0.0 };
                    if !x.is_finite() {
                        *x = 0.0;
                    }
                }
                forward.process(&mut buffer, &mut spectrum).expect("buffers of the planned size");
                for (bin, &value) in spectrum.iter().enumerate() {
                    y[(bin * span + j) * channels + c] = value;
                }
            }
        }
        // As nara_wpe, no frame's power counts as less than 1e-10 of the loudest one's.
        let floor = y.iter().map(|v| v.norm_sqr() as f64).fold(0.0, f64::max) * POWER_FLOOR;
        let mut x = vec![Complex32::ZERO; BINS * found * channels];
        for (step, (y, x)) in
            y.chunks(BANDS_PER_STEP * span * channels).zip(x.chunks_mut(BANDS_PER_STEP * found * channels)).enumerate()
        {
            let bands = y.len() / (span * channels);
            let per_thread = bands.div_ceil(threads);
            std::thread::scope(|scope| {
                for (y, x) in y.chunks(per_thread * span * channels).zip(x.chunks_mut(per_thread * found * channels)) {
                    scope.spawn(move || {
                        for (y, x) in y.chunks(span * channels).zip(x.chunks_mut(found * channels)) {
                            band(y, x, channels, decay, floor);
                        }
                    });
                }
            });
            let done = (step + 1) * BANDS_PER_STEP;
            progress((first as f32 + count as f32 * done.min(BINS) as f32 / BINS as f32) / frames as f32)?;
        }
        // Back to sound: each frame's spectrum through the inverse transform, overlapped and added.
        let mut sound = vec![vec![0f32; count * HOP + FFT - HOP]; channels];
        for (c, sound) in sound.iter_mut().enumerate() {
            sound[..FFT - HOP].copy_from_slice(&overlap[c]);
            for j in 0..count {
                for (bin, value) in spectrum.iter_mut().enumerate() {
                    *value = x[(bin * found + first - from + j) * channels + c];
                }
                // The inverse transform takes a real DC and Nyquist band.
                spectrum[0].im = 0.0;
                spectrum[BINS - 1].im = 0.0;
                inverse.process(&mut spectrum, &mut buffer).expect("buffers of the planned size");
                for (n, (s, b)) in sound[j * HOP..j * HOP + FFT].iter_mut().zip(&buffer).enumerate() {
                    *s += b * synthesis[n];
                }
            }
            overlap[c].copy_from_slice(&sound[count * HOP..]);
        }
        // Samples before the next frame's start have all their frames.
        let origin = start(first as i64);
        let done = (start((first + count) as i64).max(0) as usize).min(len);
        interleaved.clear();
        for i in written..done {
            let at = (i as i64 - origin) as usize;
            interleaved.extend((0..CHANNELS).map(|c| sound[c.min(channels - 1)][at]));
        }
        written = done;
        out(&interleaved)?;
    }
    debug_assert_eq!(written, len);
    Ok(())
}

/// How much of a band's level is left `LATE_FRAMES` later in a room with reverb time `t60`, as power.
fn late_decay(t60: f64) -> f64 {
    let seconds = (LATE_FRAMES * HOP) as f64 / SAMPLE_RATE as f64;
    (-2.0 * 3.0 * std::f64::consts::LN_10 / t60 * seconds).exp()
}

/// The reverb time of the room in `input`, from how fast its sound dies away where a phrase stops: the
/// median over phrase ends of a line fitted to the 10 ms level between 5 and 25 dB under where it stopped.
/// A phrase stops where the level has a peak within 20 dB of the speech and is 3 dB lower or more 100 to
/// 300 ms later; its decay lasts while no 10 ms is 1 dB louder than the one before. On recordings with a
/// reverb time of 0.4 and 0.8 s this read 0.42 and 0.82 s, and 0.3 s on dry speech.
fn reverb_time(input: &[f32]) -> f64 {
    let step = SAMPLE_RATE as usize / 100 * CHANNELS;
    let level: Vec<f64> = input
        .chunks(step)
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| {
            let power = w.iter().flat_map(|c| c.iter()).map(|&s| (s as f64).powi(2)).sum::<f64>();
            10.0 * (power / (2 * step) as f64 + 1e-12).log10()
        })
        .collect();
    let mut sorted = level.clone();
    sorted.sort_by(f64::total_cmp);
    let speech = sorted.get(sorted.len() * 95 / 100).copied().unwrap_or(0.0);
    let mut times = Vec::new();
    let mut e = 1;
    while e + 30 < level.len() {
        let stops = level[e] >= speech - 20.0
            && level[e] >= level[e - 1]
            && level[e] > level[e + 1]
            && level[e + 10..=e + 30].iter().all(|&l| l < level[e] - 3.0);
        if !stops {
            e += 1;
            continue;
        }
        let mut lowest = e;
        let mut j = e;
        while j + 1 < level.len() && level[j + 1] < level[j] + 1.0 {
            j += 1;
            if level[j] < level[lowest] {
                lowest = j;
            }
        }
        let points: Vec<(f64, f64)> = (e..=lowest)
            .filter(|&k| level[k] < level[e] - 5.0 && level[k] > level[e] - 25.0)
            .map(|k| (k as f64, level[k]))
            .collect();
        if level[e] - level[lowest] >= 20.0 && points.len() >= 5 {
            let n = points.len() as f64;
            let (mx, my) = points.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x / n, b + y / n));
            let (sxy, sxx) =
                points.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + (x - mx) * (y - my), b + (x - mx).powi(2)));
            // dB per 10 ms.
            let slope = sxy / sxx;
            if slope < 0.0 {
                times.push(-60.0 / (slope * 100.0));
            }
        }
        e = lowest.max(e + 1);
    }
    if times.is_empty() {
        return T60_DEFAULT;
    }
    times.sort_by(f64::total_cmp);
    times[times.len() / 2].clamp(T60_RANGE.0, T60_RANGE.1)
}

/// One band: `y` holds `HISTORY` frames before the ones to process and then them, `x` gets those
/// without reverb. WPE weights every frame by the inverse of its power without reverb as the pass before
/// found it, finds the filter that best predicts the frame from `TAPS` frames `DELAY` frames earlier, and
/// takes the prediction away. Then each frame is turned down by how much of it is the room's decay of
/// the frames before (`decay`), as a Wiener gain with decision-directed smoothing.
fn band(y: &[Complex32], x: &mut [Complex32], channels: usize, decay: f64, power_floor: f64) {
    let frames = x.len() / channels;
    let n = TAPS * channels;
    x.copy_from_slice(&y[HISTORY * channels..]);
    // Split into real and imaginary parts, so the sums over the taps vectorise.
    let (mut re, mut im) = (vec![0.0f64; n * n], vec![0.0f64; n * n]);
    let (mut cross_re, mut cross_im) = (vec![0.0f64; n * channels], vec![0.0f64; n * channels]);
    let (mut window_re, mut window_im) = (vec![0.0f64; n], vec![0.0f64; n]);
    // The frames a prediction of frame `t` reads, oldest first.
    let past = |t: usize| &y[t * channels..(t + TAPS) * channels];
    for _ in 0..ITERATIONS {
        re.fill(0.0);
        im.fill(0.0);
        cross_re.fill(0.0);
        cross_im.fill(0.0);
        for t in 0..frames {
            let power =
                x[t * channels..(t + 1) * channels].iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / channels as f64;
            let weight = 1.0 / power.max(power_floor).max(f64::MIN_POSITIVE);
            for (k, v) in past(t).iter().enumerate() {
                (window_re[k], window_im[k]) = (v.re as f64, v.im as f64);
            }
            let now = &y[(t + HISTORY) * channels..(t + HISTORY + 1) * channels];
            for i in 0..n {
                let (ar, ai) = (window_re[i] * weight, window_im[i] * weight);
                let (row_re, row_im) = (&mut re[i * n..(i + 1) * n], &mut im[i * n..(i + 1) * n]);
                // a times the conjugate of the window.
                for j in i..n {
                    row_re[j] += ar * window_re[j] + ai * window_im[j];
                    row_im[j] += ai * window_re[j] - ar * window_im[j];
                }
                for (c, v) in now.iter().enumerate() {
                    cross_re[i * channels + c] += ar * v.re as f64 + ai * v.im as f64;
                    cross_im[i * channels + c] += ai * v.re as f64 - ar * v.im as f64;
                }
            }
        }
        for i in 0..n {
            for j in 0..i {
                (re[i * n + j], im[i * n + j]) = (re[j * n + i], -im[j * n + i]);
            }
        }
        solve(&mut re, &mut im, &mut cross_re, &mut cross_im, n, channels);
        for t in 0..frames {
            let window = past(t);
            for c in 0..channels {
                let (mut pr, mut pi) = (0.0f64, 0.0f64);
                for (i, v) in window.iter().enumerate() {
                    // The conjugate of the filter times the window.
                    let (gr, gi) = (cross_re[i * channels + c], -cross_im[i * channels + c]);
                    pr += gr * v.re as f64 - gi * v.im as f64;
                    pi += gr * v.im as f64 + gi * v.re as f64;
                }
                let now = y[(t + HISTORY) * channels + c];
                x[t * channels + c] = Complex32::new(now.re - pr as f32, now.im - pi as f32);
            }
        }
    }
    let smoothing = (-(HOP as f64) / (SAMPLE_RATE as f64 * SMOOTHING_S)).exp();
    let floor = 10f64.powf(FLOOR_DB / 20.0);
    let mut smoothed = vec![0.0f64; frames];
    let mut clean = 0.0;
    for t in 0..frames {
        let power =
            x[t * channels..(t + 1) * channels].iter().map(|v| v.norm_sqr() as f64).sum::<f64>() / channels as f64;
        smoothed[t] = if t > 0 { smoothing * smoothed[t - 1] + (1.0 - smoothing) * power } else { power };
        let late = if t >= LATE_FRAMES { decay * smoothed[t - LATE_FRAMES] } else { 0.0 }
            .max(power_floor)
            .max(f64::MIN_POSITIVE);
        // From the smoothed level, so steady sound such as room tone gets a steady gain, not a fluttering one.
        let ratio = DIRECTED * clean / late + (1.0 - DIRECTED) * (smoothed[t] / late - 1.0).max(0.0);
        let gain = (ratio / (1.0 + ratio)).max(floor);
        clean = gain * gain * smoothed[t];
        for v in &mut x[t * channels..(t + 1) * channels] {
            *v *= gain as f32;
        }
    }
}

/// Solves `a g = b` in place for the Hermitian positive definite `a` (`n` by `n`) and `b` (`n` by
/// `m`), each as real and imaginary parts, by Cholesky; `b` becomes `g`.
fn solve(are: &mut [f64], aim: &mut [f64], bre: &mut [f64], bim: &mut [f64], n: usize, m: usize) {
    let mean = (0..n).map(|i| are[i * n + i]).sum::<f64>() / n as f64;
    for i in 0..n {
        are[i * n + i] += mean * LOADING + f64::MIN_POSITIVE;
    }
    // The lower triangle of `a` becomes L, with a = L L^H.
    for j in 0..n {
        let diagonal = (are[j * n + j] - (0..j).map(|k| are[j * n + k].powi(2) + aim[j * n + k].powi(2)).sum::<f64>())
            .max(f64::MIN_POSITIVE);
        let l = diagonal.sqrt();
        (are[j * n + j], aim[j * n + j]) = (l, 0.0);
        for i in j + 1..n {
            // Sum of L[i][k] times the conjugate of L[j][k].
            let (mut sr, mut si) = (0.0, 0.0);
            for k in 0..j {
                let (a, b, c, d) = (are[i * n + k], aim[i * n + k], are[j * n + k], aim[j * n + k]);
                sr += a * c + b * d;
                si += b * c - a * d;
            }
            are[i * n + j] = (are[i * n + j] - sr) / l;
            aim[i * n + j] = (aim[i * n + j] - si) / l;
        }
    }
    for c in 0..m {
        for i in 0..n {
            let (mut sr, mut si) = (0.0, 0.0);
            for k in 0..i {
                let (a, b, gr, gi) = (are[i * n + k], aim[i * n + k], bre[k * m + c], bim[k * m + c]);
                sr += a * gr - b * gi;
                si += a * gi + b * gr;
            }
            bre[i * m + c] = (bre[i * m + c] - sr) / are[i * n + i];
            bim[i * m + c] = (bim[i * m + c] - si) / are[i * n + i];
        }
        for i in (0..n).rev() {
            // The conjugate transpose of L.
            let (mut sr, mut si) = (0.0, 0.0);
            for k in i + 1..n {
                let (a, b, gr, gi) = (are[k * n + i], -aim[k * n + i], bre[k * m + c], bim[k * m + c]);
                sr += a * gr - b * gi;
                si += a * gi + b * gr;
            }
            bre[i * m + c] = (bre[i * m + c] - sr) / are[i * n + i];
            bim[i * m + c] = (bim[i * m + c] - si) / are[i * n + i];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SAMPLE_RATE;
    use std::f64::consts::TAU;

    const RATE: f64 = SAMPLE_RATE as f64;

    /// Deterministic white noise in -1..1.
    struct Noise(u64);
    impl Noise {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 52) as f64 - 1.0
        }
    }

    /// A sung "a" at `f0` Hz: a glottal pulse train through three formant resonators.
    fn vowel(n: usize, f0: f64) -> Vec<f64> {
        let mut resonators: Vec<(f64, f64, f64, [f64; 2])> =
            [(700.0, 80.0, 1.0), (1220.0, 90.0, 0.5), (2600.0, 120.0, 0.25)]
                .iter()
                .map(|&(hz, bandwidth, gain): &(f64, f64, f64)| {
                    let r = (-std::f64::consts::PI * bandwidth / RATE).exp();
                    (2.0 * r * (TAU * hz / RATE).cos(), -r * r, gain, [0.0; 2])
                })
                .collect();
        let mut phase = 0.0;
        (0..n)
            .map(|i| {
                phase += (f0 + 8.0 * (TAU * 5.0 * i as f64 / RATE).sin()) / RATE;
                let pulse = if phase >= 1.0 {
                    phase -= 1.0;
                    1.0
                } else {
                    0.0
                };
                resonators
                    .iter_mut()
                    .map(|(a1, a2, gain, y)| {
                        let out = pulse + *a1 * y[0] + *a2 * y[1];
                        *y = [out, y[0]];
                        out * *gain
                    })
                    .sum::<f64>()
            })
            .collect()
    }

    fn convolve(a: &[f64], b: &[f64]) -> Vec<f64> {
        let size = (a.len() + b.len()).next_power_of_two();
        let mut planner = RealFftPlanner::<f64>::new();
        let (forward, inverse) = (planner.plan_fft_forward(size), planner.plan_fft_inverse(size));
        let spectrum = |x: &[f64]| {
            let mut input = forward.make_input_vec();
            input[..x.len()].copy_from_slice(x);
            let mut out = forward.make_output_vec();
            forward.process(&mut input, &mut out).unwrap();
            out
        };
        let mut product: Vec<_> = spectrum(a).iter().zip(spectrum(b)).map(|(x, y)| x * y).collect();
        let mut out = inverse.make_output_vec();
        inverse.process(&mut product, &mut out).unwrap();
        out.truncate(a.len());
        out.iter().map(|x| x / size as f64).collect()
    }

    /// A room: the direct sound, three early reflections within 21 ms and a diffuse tail that decays
    /// by 60 dB in `t60` seconds, as loud as the direct sound.
    fn room(t60: f64, seed: u64) -> Vec<f64> {
        let mut noise = Noise(seed);
        let tail = (13.8 * 3.0 / (RATE * t60)).sqrt();
        let mut response: Vec<f64> = (0..(RATE * t60) as usize)
            .map(|i| if i < 240 { 0.0 } else { tail * noise.next() * (-6.9 * i as f64 / RATE / t60).exp() })
            .collect();
        for (ms, gain) in [(0.0, 1.0), (7.0, 0.5), (13.0, -0.35), (21.0, 0.3)] {
            response[(ms * RATE / 1000.0) as usize] += gain;
        }
        response
    }

    fn run(input: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        let collect = |frames: &[f32]| {
            out.extend_from_slice(frames);
            Ok(())
        };
        dereverb(input, collect, |_| Ok(())).unwrap();
        out
    }

    fn energy(x: &[f64]) -> f64 {
        x.iter().map(|s| s * s).sum()
    }

    /// Eight phrases of four syllables each with pauses between them, in a room with 0.6 s of reverb:
    /// what rings on after each phrase drops, the phrases keep their level and nothing moves in time.
    #[test]
    fn the_tail_after_each_phrase_drops_and_the_voice_stays() {
        let (syllable, gap, pause) = (RATE as usize * 18 / 100, RATE as usize * 7 / 100, RATE as usize * 7 / 10);
        let mut dry = vec![0.0; pause];
        let mut phrases = Vec::new();
        for k in 0..8 {
            let start = dry.len();
            for s in 0..4 {
                let voice = vowel(syllable, 110.0 + 15.0 * ((k + s) % 5) as f64);
                let peak = voice.iter().fold(0.0f64, |m, s| m.max(s.abs()));
                dry.extend(voice.iter().map(|s| s / peak * 0.2));
                dry.extend(std::iter::repeat_n(0.0, gap));
            }
            dry.truncate(dry.len() - gap);
            phrases.push((start, dry.len()));
            dry.extend(std::iter::repeat_n(0.0, pause));
        }
        let wet = convolve(&dry, &room(0.6, 9));
        let input: Vec<f32> = wet.iter().flat_map(|&s| [s as f32; CHANNELS]).collect();
        let started = std::time::Instant::now();
        let output = run(&input);
        let seconds = started.elapsed().as_secs_f64();
        assert_eq!(output.len(), input.len());
        let output: Vec<f64> = output.iter().step_by(CHANNELS).map(|&s| s as f64).collect();
        // Phrases after the first, which only has itself to learn from in the first block.
        let ratio = |x: &[f64]| {
            let (tail, voice) = phrases[1..].iter().fold((0.0, 0.0), |(tail, voice), &(a, b)| {
                (tail + energy(&x[b + 2_400..b + 14_400]), voice + energy(&x[a..b]))
            });
            (10.0 * (tail / voice).log10(), 10.0 * voice.log10())
        };
        let ((tail_in, voice_in), (tail_out, voice_out)) = (ratio(&wet), ratio(&output));
        eprintln!(
            "dereverb: tail {tail_in:.1} -> {tail_out:.1} dB under the phrases, phrases {:+.2} dB, {:.0}x real time",
            voice_out - voice_in,
            dry.len() as f64 / RATE / seconds
        );
        assert!(tail_out - tail_in < -6.0, "the tail dropped only {:.1} dB", tail_out - tail_in);
        assert!((voice_out - voice_in).abs() < 3.0, "the phrases changed by {:.2} dB", voice_out - voice_in);
        // In step with the sound it was made from.
        let (a, b) = phrases[3];
        let lag = (-200i64..=200)
            .max_by(|&x, &y| {
                let corr = |lag: i64| (a..b).map(|i| wet[i] * output[(i as i64 + lag) as usize]).sum::<f64>();
                corr(x).total_cmp(&corr(y))
            })
            .unwrap();
        assert_eq!(lag, 0, "the voice is {lag} samples off");
    }

    /// Without reverb there is nothing to predict: sound passes nearly as it is, as long as it was,
    /// a click stays on its sample, and the same input gives the same output.
    #[test]
    fn dry_sound_passes_with_its_length_and_timing() {
        for frames in [0, 1, 383, 384, 1_536, 12_345, BLOCK * HOP + 777] {
            let input: Vec<f32> = (0..frames * CHANNELS).map(|i| ((i * 7919) % 1000) as f32 / 5000.0 - 0.1).collect();
            assert_eq!(run(&input).len(), input.len(), "{frames} frames");
        }
        let mut click = vec![0f32; SAMPLE_RATE as usize * CHANNELS];
        click[20_000 * CHANNELS] = 0.5;
        click[20_000 * CHANNELS + 1] = 0.5;
        let output = run(&click);
        let loudest = (0..output.len() / CHANNELS).max_by(|&a, &b| output[a * 2].abs().total_cmp(&output[b * 2].abs()));
        assert_eq!(loudest, Some(20_000), "the click moved");
        let mut noise = Noise(5);
        let stereo: Vec<f32> = (0..SAMPLE_RATE as usize * 3 * CHANNELS).map(|_| (noise.next() * 0.1) as f32).collect();
        let first = run(&stereo);
        assert!(first.iter().zip(run(&stereo)).all(|(a, b)| a.to_bits() == b.to_bits()), "same input, other output");
        // Steady room tone looks a little like its own reverb and comes down a few dB, with its sound kept:
        // what changes beyond the level is what the filters fit of it, 30 coefficients over a block.
        let tone: Vec<f32> =
            (0..SAMPLE_RATE as usize * 20).flat_map(|_| [(noise.next() * 0.01) as f32; CHANNELS]).collect();
        let output = run(&tone);
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(&a, &b)| a as f64 * b as f64).sum::<f64>();
        let scale = dot(&output, &tone) / dot(&tone, &tone);
        let error = tone.iter().zip(&output).map(|(&a, &b)| (b as f64 - scale * a as f64).powi(2)).sum::<f64>();
        let change = 10.0 * (error / (scale * scale * dot(&tone, &tone))).log10();
        eprintln!("room tone: {:+.1} dB, changed by {change:.1} dB", 20.0 * scale.log10());
        assert!(change < -12.0 && scale > 0.6, "room tone changed by {change:.1} dB at {scale:.2}");
    }

    #[test]
    fn a_stop_request_ends_the_work_within_a_block() {
        let input = vec![0.1f32; SAMPLE_RATE as usize * 40 * CHANNELS];
        let mut written = 0;
        let count = |frames: &[f32]| {
            written += frames.len() / CHANNELS;
            Ok(())
        };
        let error = dereverb(&input, count, |_| anyhow::bail!("CANCELLED: stop")).unwrap_err();
        assert!(error.to_string().starts_with("CANCELLED"));
        assert!(written <= BLOCK * HOP, "{written} frames processed after the stop");
    }
}
