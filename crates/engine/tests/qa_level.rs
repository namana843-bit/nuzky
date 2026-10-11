//! Reduce echo, Match loudness and Even out volume end to end in the engine: a clip with a setting plays
//! and exports the sound its file's caches and measured blocks give, every sound on the sample it had,
//! and a project without them sounds bit for bit as before.
mod qa_support;
use nuzky_engine::{
    audio::{Chain, Mixer, pcm_path},
    edit::{EditCmd, TimeRange},
    export::{ExportOptions, export},
    level,
    loudness::Meter,
    media::probe,
    model::*,
};
use qa_support::*;
use realfft::RealFftPlanner;
use std::{f64::consts::TAU, path::Path, process::Command, sync::atomic::AtomicBool};

const RATE: f64 = SAMPLE_RATE as f64;
const SECOND: usize = SAMPLE_RATE as usize;

/// Writes 48 kHz 32-bit float WAV, the same sound on both channels.
fn wav(path: &Path, samples: &[f64]) {
    let data = (samples.len() * CHANNELS * 4) as u32;
    let mut bytes = Vec::with_capacity(44 + data as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes());
    bytes.extend_from_slice(&(CHANNELS as u16).to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * CHANNELS as u32 * 4).to_le_bytes());
    bytes.extend_from_slice(&(CHANNELS as u16 * 4).to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    for &s in samples {
        for _ in 0..CHANNELS {
            bytes.extend_from_slice(&(s as f32).to_le_bytes());
        }
    }
    std::fs::write(path, bytes).unwrap();
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
    let voice: Vec<f64> = (0..n)
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
        .collect();
    let peak = voice.iter().fold(0.0f64, |m, s| m.max(s.abs()));
    voice.iter().map(|s| s / peak).collect()
}

/// Phrases of four syllables at the levels `levels` (peak, linear), each followed by `pause` seconds, and
/// where each phrase is.
fn speech(levels: &[f64], pause: f64) -> (Vec<f64>, Vec<(usize, usize)>) {
    let (syllable, gap) = (SECOND * 18 / 100, SECOND * 7 / 100);
    let mut out = vec![0.0; (pause * RATE) as usize];
    let mut phrases = Vec::new();
    for (k, level) in levels.iter().enumerate() {
        let start = out.len();
        for s in 0..4 {
            out.extend(vowel(syllable, 110.0 + 15.0 * ((k + s) % 5) as f64).iter().map(|v| v * level));
            out.extend(std::iter::repeat_n(0.0, gap));
        }
        out.truncate(out.len() - gap);
        phrases.push((start, out.len()));
        out.extend(std::iter::repeat_n(0.0, (pause * RATE) as usize));
    }
    (out, phrases)
}

/// `dry` in a room with 0.6 s of reverb: the direct sound, early reflections within 21 ms and a diffuse
/// tail as loud as the direct sound.
fn in_a_room(dry: &[f64]) -> Vec<f64> {
    let mut rng = Rng(9);
    let (t60, tail) = (0.6, (13.8 * 3.0 / (RATE * 0.6)).sqrt());
    let mut response: Vec<f64> = (0..(RATE * t60) as usize)
        .map(|i| {
            let noise = (rng.next() >> 11) as f64 / (1u64 << 52) as f64 - 1.0;
            if i < 240 { 0.0 } else { tail * noise * (-6.9 * i as f64 / RATE / t60).exp() }
        })
        .collect();
    for (ms, gain) in [(0.0, 1.0), (7.0, 0.5), (13.0, -0.35), (21.0, 0.3)] {
        response[(ms * RATE / 1000.0) as usize] += gain;
    }
    let size = (dry.len() + response.len()).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let (forward, inverse) = (planner.plan_fft_forward(size), planner.plan_fft_inverse(size));
    let spectrum = |x: &[f64]| {
        let mut input = forward.make_input_vec();
        input[..x.len()].copy_from_slice(x);
        let mut out = forward.make_output_vec();
        forward.process(&mut input, &mut out).unwrap();
        out
    };
    let mut product: Vec<_> = spectrum(dry).iter().zip(spectrum(&response)).map(|(x, y)| x * y).collect();
    let mut out = inverse.make_output_vec();
    inverse.process(&mut product, &mut out).unwrap();
    out[..dry.len()].iter().map(|x| x / size as f64).collect()
}

fn media_project(sources: &[(&Path, &str)]) -> Project {
    let mut p = Project::new("voice");
    let assets: Vec<Asset> = sources.iter().map(|(path, id)| probe(path, (*id).into()).unwrap()).collect();
    p.apply(EditCmd::AddAssets { assets: assets.clone() }).unwrap();
    // One after the other, as takes of a video are.
    let mut start_us = 0;
    for asset in &assets {
        p.apply(EditCmd::AddClip { asset_id: asset.id.clone(), start_us: Some(start_us), track_id: None }).unwrap();
        start_us += asset.duration_us;
    }
    p
}

fn set(project: &Project, settings: serde_json::Value) -> Project {
    let mut p = project.clone();
    for id in p.tracks.iter().flat_map(|t| &t.clips).map(|c| c.id.clone()).collect::<Vec<_>>() {
        let mut json = serde_json::json!({"type": "updateClip", "clipId": id});
        json.as_object_mut().unwrap().extend(settings.as_object().unwrap().clone());
        p.apply(serde_json::from_value(json).unwrap()).unwrap();
    }
    p
}

/// The left channel of the timeline mix from `from` for `frames`, in one go.
fn mix(cache: &Path, project: &Project, from: usize, frames: usize) -> Vec<f32> {
    let mut out = vec![0.0; frames * CHANNELS];
    Mixer::new(cache.to_path_buf()).mix(project, from as i64, &mut out);
    out
}

fn left(stereo: &[f32]) -> Vec<f64> {
    stereo.iter().step_by(CHANNELS).map(|&s| s as f64).collect()
}

/// The decoded sound of an exported file, 48 kHz stereo.
fn decode(path: &Path) -> Vec<f32> {
    let out = run(Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-f", "f32le", "-ac", "2", "-ar", "48000", "-"]));
    out.stdout.as_chunks::<4>().0.iter().map(|&b| f32::from_le_bytes(b)).collect()
}

fn lufs(stereo: &[f32]) -> f64 {
    let mut meter = Meter::new();
    meter.push(stereo);
    meter.finish().integrated.unwrap()
}

/// Energy 50-300 ms after each phrase against the phrases', in dB.
fn tail_db(x: &[f64], phrases: &[(usize, usize)]) -> f64 {
    let energy = |a: usize, b: usize| x[a..b.min(x.len())].iter().map(|s| s * s).sum::<f64>();
    let (tail, voice) =
        phrases.iter().fold((0.0, 0.0), |(t, v), &(a, b)| (t + energy(b + 2_400, b + 14_400), v + energy(a, b)));
    10.0 * (tail / voice).log10()
}

fn options() -> ExportOptions {
    ExportOptions { preset: "ultrafast".into(), replace_existing: true, ..ExportOptions::default() }
}

/// A video of a talking head in a ringing room, cut in a pause: Reduce echo plays and exports the sound
/// without reverb, Reduce by mixes it with the original, and every sound stays on its sample.
#[test]
fn reduce_echo_takes_the_tail_out_and_keeps_every_sound_on_its_sample() {
    if !available() {
        return;
    }
    let d = dir(&format!("level-echo-{}", nuzky_engine::edit::new_id()));
    let (dry, phrases) = speech(&[0.3; 8], 0.7);
    let source = d.join("room.wav");
    wav(&source, &in_a_room(&dry));
    let cache = d.join("cache");
    let mut p = media_project(&[(&source, "room")]);
    // A cut inside the pause after the fourth phrase: 0.3 s of it goes.
    let at = phrases[3].1 + SECOND / 5;
    let us = |frames: usize| (frames as i64 * 1_000_000) / RATE as i64;
    let cut = TimeRange { start_us: us(at), end_us: us(at + SECOND * 3 / 10) };
    p.apply(EditCmd::RippleDeleteRanges { ranges: vec![cut], keep_track_ids: Some(vec![]) }).unwrap();
    // On the timeline the phrases after the cut come 0.3 s earlier.
    let placed: Vec<(usize, usize)> = phrases
        .iter()
        .map(|&(a, b)| if a > at { (a - SECOND * 3 / 10, b - SECOND * 3 / 10) } else { (a, b) })
        .collect();
    let total = dry.len() - SECOND * 3 / 10;
    let echo = set(&p, serde_json::json!({"dereverb": 1.0}));
    let asset = p.assets[0].clone();
    let raw = Chain { dereverb: false, clean_voice: false }.ensure(&cache, &asset, |_| Ok(())).unwrap();
    let plain = mix(&cache, &p, 0, total);
    // Until the sound without reverb is there, the clips play the original.
    assert!(mix(&cache, &echo, 0, total) == plain, "silence or another sound while it is prepared");
    let without = Chain { dereverb: true, clean_voice: false }.ensure(&cache, &asset, |_| Ok(())).unwrap();
    assert_eq!(
        std::fs::metadata(&without).unwrap().len(),
        std::fs::metadata(&raw).unwrap().len(),
        "as long as the raw sound"
    );
    assert_eq!(raw, pcm_path(&cache, &asset));
    let full = mix(&cache, &echo, 0, total);
    let (before, after) = (tail_db(&left(&plain), &placed[1..]), tail_db(&left(&full), &placed[1..]));
    eprintln!("QA echo: tail after the phrases {before:.1} dB, with Reduce echo {after:.1} dB");
    assert!(after < before - 6.0, "the tail dropped only {:.1} dB", before - after);
    // Reduce by mixes the two in step: half way is the average, sample for sample.
    let half = mix(&cache, &set(&p, serde_json::json!({"dereverb": 0.5})), 0, total);
    let worst =
        half.iter().zip(plain.iter().zip(&full)).map(|(h, (a, b))| (h - (a + b) / 2.0).abs()).fold(0f32, f32::max);
    assert!(worst < 1e-5, "half way differs by {worst}");
    // Every phrase, before and after the cut, lines up with the original at lag 0.
    for &(a, b) in &[placed[2], placed[5]] {
        let (x, y) = (left(&plain), left(&full));
        let lag = (-480i64..=480)
            .max_by(|&l, &m| {
                let corr = |lag: i64| (a..b).map(|i| x[i] * y[(i as i64 + lag) as usize]).sum::<f64>();
                corr(l).total_cmp(&corr(m))
            })
            .unwrap();
        assert_eq!(lag, 0, "the phrase at {a} moved by {lag} samples");
    }
    // The export has it too, at the same length, and makes it itself when the app has not.
    std::fs::remove_dir_all(cache.join("dereverb")).unwrap();
    let (off, on) = (d.join("off.mp4"), d.join("on.mp4"));
    export(&p, &cache, &off, &options(), &AtomicBool::new(false), |_| {}).unwrap();
    export(&echo, &cache, &on, &options(), &AtomicBool::new(false), |_| {}).unwrap();
    let (off, on) = (decode(&off), decode(&on));
    assert_eq!(off.len(), on.len(), "the exports differ in length");
    let (before, after) = (tail_db(&left(&off), &placed[1..]), tail_db(&left(&on), &placed[1..]));
    eprintln!("QA echo: export tail {before:.1} dB, with Reduce echo {after:.1} dB");
    assert!(after < before - 5.0, "the exported tail dropped only {:.1} dB", before - after);
    std::fs::remove_dir_all(d).unwrap();
}

/// Two takes 10 dB apart: with Match loudness both play and export within 1 LU of each other, at -16 LUFS.
#[test]
fn match_loudness_brings_takes_10_db_apart_within_1_lu() {
    if !available() {
        return;
    }
    let d = dir(&format!("level-match-{}", nuzky_engine::edit::new_id()));
    let (loud, quiet) = (d.join("loud.wav"), d.join("quiet.wav"));
    let (voice, _) = speech(&[0.5, 0.45, 0.5, 0.4, 0.5, 0.45], 0.3);
    wav(&loud, &voice);
    wav(&quiet, &voice.iter().map(|s| s * 10f64.powf(-0.5)).collect::<Vec<_>>());
    let cache = d.join("cache");
    let p = media_project(&[(&loud, "loud"), (&quiet, "quiet")]);
    let matched = set(&p, serde_json::json!({"normalize": true}));
    let take = voice.len();
    let takes = |sound: &[f32]| [lufs(&sound[..take * CHANNELS]), lufs(&sound[take * CHANNELS..2 * take * CHANNELS])];
    for asset in &p.assets {
        let path = Chain { dereverb: false, clean_voice: false }.ensure(&cache, asset, |_| Ok(())).unwrap();
        level::ensure_blocks(&path, |_| Ok(())).unwrap();
    }
    let [a, b] = takes(&mix(&cache, &p, 0, 2 * take));
    let [x, y] = takes(&mix(&cache, &matched, 0, 2 * take));
    eprintln!("QA match: takes at {a:.1} and {b:.1} LUFS, matched {x:.1} and {y:.1} LUFS");
    assert!((a - b - 10.0).abs() < 0.2);
    assert!((x - y).abs() < 1.0 && (x - level::MATCH_LUFS).abs() < 1.0 && (y - level::MATCH_LUFS).abs() < 1.0);
    let gains: Vec<f64> = matched
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .map(|clip| nuzky_engine::audio::match_gain_db(&cache, &matched, clip).unwrap())
        .collect();
    assert!((gains[1] - gains[0] - 10.0).abs() < 0.2, "the gains the inspector shows: {gains:?}");
    // Export measures the files itself and lands where playback does.
    for asset in &p.assets {
        std::fs::remove_file(level::blocks_path(&pcm_path(&cache, asset))).unwrap();
    }
    let out = d.join("matched.mp4");
    export(&matched, &cache, &out, &options(), &AtomicBool::new(false), |_| {}).unwrap();
    let [x, y] = takes(&decode(&out));
    eprintln!("QA match: exported takes at {x:.1} and {y:.1} LUFS");
    assert!((x - y).abs() < 1.0 && (x - level::MATCH_LUFS).abs() < 1.0);
    std::fs::remove_dir_all(d).unwrap();
}

/// Passages 12 dB apart in one take come within a few dB with Even out volume, without moving a sample.
#[test]
fn even_out_volume_narrows_the_spread_of_passages() {
    if !available() {
        return;
    }
    let d = dir(&format!("level-even-{}", nuzky_engine::edit::new_id()));
    let source = d.join("drifting.wav");
    // Four passages of three phrases each, a loud one, then one 12 dB quieter, and again.
    let levels: Vec<f64> = (0..12).map(|k| if k / 3 % 2 == 0 { 0.5 } else { 0.125 }).collect();
    let (voice, phrases) = speech(&levels, 0.25);
    wav(&source, &voice);
    let cache = d.join("cache");
    let p = media_project(&[(&source, "drifting")]);
    let even = set(&p, serde_json::json!({"leveler": 0.8}));
    let path = Chain { dereverb: false, clean_voice: false }.ensure(&cache, &p.assets[0], |_| Ok(())).unwrap();
    level::ensure_blocks(&path, |_| Ok(())).unwrap();
    let spread = |sound: &[f32]| {
        let passages: Vec<f64> =
            phrases.chunks(3).map(|p| lufs(&sound[p[0].0 * CHANNELS..p[2].1 * CHANNELS])).collect();
        passages.iter().fold(f64::MIN, |m, &l| m.max(l)) - passages.iter().fold(f64::MAX, |m, &l| m.min(l))
    };
    let (plain, evened) = (mix(&cache, &p, 0, voice.len()), mix(&cache, &even, 0, voice.len()));
    let (before, after) = (spread(&plain), spread(&evened));
    eprintln!("QA even: passages {before:.1} dB apart, evened out {after:.1} dB");
    assert!(before > 11.0 && after < before / 2.0, "{before:.1} dB to {after:.1} dB");
    // A gain, nothing else: where the gain is 1 the samples are the original's.
    let ratio: Vec<f64> =
        left(&evened).iter().zip(left(&plain)).filter(|(_, b)| b.abs() > 0.01).map(|(a, b)| a / b).collect();
    let jumps = ratio.windows(2).filter(|w| (w[1] - w[0]).abs() > 0.01).count();
    assert!(jumps < ratio.len() / 1000, "{jumps} steps in the gain");
    std::fs::remove_dir_all(d).unwrap();
}

/// A project saved before these settings opens with them off and plays the raw sound bit for bit, even
/// with every processed cache and measurement of its file at hand.
#[test]
fn a_project_from_before_plays_bit_for_bit_as_before() {
    if !available() {
        return;
    }
    let d = dir(&format!("level-old-{}", nuzky_engine::edit::new_id()));
    let source = d.join("take.wav");
    let (voice, _) = speech(&[0.4, 0.2, 0.4], 0.3);
    wav(&source, &in_a_room(&voice));
    let cache = d.join("cache");
    let asset = probe(&source, "take".into()).unwrap();
    for chain in [(false, false), (true, false), (false, true), (true, true)] {
        let path = Chain { dereverb: chain.0, clean_voice: chain.1 }.ensure(&cache, &asset, |_| Ok(())).unwrap();
        level::ensure_blocks(&path, |_| Ok(())).unwrap();
    }
    let old = r#"{"type":"media","assetId":"take","sourceInUs":0,"volume":1.0,"cleanVoice":false}"#;
    let mut p = Project::new("old");
    p.assets.push(asset.clone());
    p.tracks[0].clips.push(Clip::new("c".into(), 0, asset.duration_us, serde_json::from_str(old).unwrap()));
    let ClipContent::Media { dereverb, normalize, leveler, .. } = &p.tracks[0].clips[0].content else { panic!() };
    assert_eq!((*dereverb, *normalize, *leveler), (0.0, false, 0.0));
    let raw = pcm(&pcm_path(&cache, &asset));
    // Past the 5 ms fade-in at the start of the clip.
    let (from, frames) = (SECOND / 10, voice.len() - SECOND / 5);
    let played = mix(&cache, &p, from, frames);
    assert!(played == raw[from * CHANNELS..(from + frames) * CHANNELS], "the old project sounds different");
    std::fs::remove_dir_all(d).unwrap();
}

/// How fast Reduce echo is on a 15-minute stereo phone recording. Run it with the engine optimised as in
/// a release: `cargo test --config 'profile.dev.package.nuzky-engine.opt-level=3' -p nuzky-engine --test
/// qa_level fifteen -- --ignored --nocapture`.
#[test]
#[ignore = "15 minutes of sound; run when Reduce echo changes"]
fn fifteen_minutes_of_stereo_lose_their_echo_faster_than_real_time() {
    if !available() {
        return;
    }
    let d = dir("level-fifteen-minutes");
    let source = d.join("stereo.m4a");
    if !source.exists() {
        let speech = "aevalsrc='0.2*sin(2*PI*(140+20*sin(2*PI*0.3*t))*t)*gt(sin(2*PI*2.5*t),0)|0.2*sin(2*PI*(150+20*sin(2*PI*0.3*t))*t)*gt(sin(2*PI*2.5*t),0)':s=48000:d=900";
        ff(&["-f", "lavfi", "-i", speech, "-af", "aecho=0.8:0.7:40|70:0.4|0.3", "-c:a", "aac"], &source);
    }
    let cache = d.join("cache");
    let asset = probe(&source, "stereo".into()).unwrap();
    Chain { dereverb: false, clean_voice: false }.ensure(&cache, &asset, |_| Ok(())).unwrap();
    std::fs::remove_dir_all(cache.join("dereverb")).ok();
    let started = std::time::Instant::now();
    Chain { dereverb: true, clean_voice: false }.ensure(&cache, &asset, |_| Ok(())).unwrap();
    let seconds = started.elapsed().as_secs_f64();
    eprintln!("QA echo: 900 s of stereo in {seconds:.1} s, {:.1}x real time", 900.0 / seconds);
    assert!(seconds < 900.0 / 2.0, "Reduce echo took {seconds:.1} s");
}
