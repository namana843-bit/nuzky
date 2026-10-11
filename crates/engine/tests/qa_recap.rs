mod qa_support;
use nuzky_engine::{
    Renderer, Wait,
    edit::{EditCmd, Editor, TimeRange},
    media::probe,
    model::*,
    recap::{Recap, RecapCut, RecapOptions, RecapWord, duration_us, render, sections},
};
use qa_support::*;
use std::{
    path::Path,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

const S: i64 = 1_000_000;

/// A 20 s vertical talking-head take with a 440 Hz tone, cut at 2–4 s and 10–12.5 s.
fn recap(dir: &Path) -> Recap {
    let take = dir.join("take.mp4");
    if !take.exists() {
        let face = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site/public/media/ugc.jpg");
        ff(
            &[
                "-loop",
                "1",
                "-framerate",
                "30",
                "-i",
                face.to_str().unwrap(),
                "-f",
                "lavfi",
                "-i",
                "sine=f=440:r=48000:d=20",
                "-vf",
                "scale=1080:1920,drawtext=text='%{n}':x=40:y=1700:fontsize=60:fontcolor=white:box=1:boxcolor=black",
                "-t",
                "20",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-c:a",
                "aac",
            ],
            &take,
        );
    }
    let mut before = Project::new("Recap QA");
    before.assets.push(probe(&take, "take".into()).unwrap());
    before.tracks[0].clips.push(clip("take-clip", "take", 0, 20 * S));
    let mut editor = Editor::new(before.clone());
    let cut = |a: i64, b: i64| TimeRange { start_us: a, end_us: b };
    editor
        .apply(
            EditCmd::RippleDeleteRanges {
                ranges: vec![cut(2 * S, 4 * S), cut(10 * S, 12_500_000)],
                keep_track_ids: None,
            },
            None,
        )
        .unwrap();
    let text = "Dneska vám ukážu jak natočit video na mobil za deset minut potřebujete jen telefon a okno \
                Okno je vaše nejlepší světlo postavte se k němu čelem a kameru dejte do výšky očí";
    let cuts = [(2 * S, 4 * S), (10 * S, 12_500_000)];
    let words: Vec<RecapWord> = text
        .split(' ')
        .enumerate()
        .map(|(i, text)| {
            let start_us = i as i64 * 600_000;
            let middle = start_us + 200_000;
            RecapWord {
                start_us,
                end_us: start_us + 400_000,
                text: text.into(),
                removed: cuts.iter().any(|&(a, b)| a <= middle && middle < b),
            }
        })
        .collect();
    let in_cut = |(a, b): (i64, i64)| {
        words.iter().filter(|w| w.removed && a <= w.start_us + 200_000 && w.start_us + 200_000 < b).count()
    };
    Recap {
        cuts: vec![
            RecapCut { start_us: 2 * S, end_us: 4 * S, words: in_cut(cuts[0]), retakes: 1 },
            RecapCut { start_us: 10 * S, end_us: 12_500_000, words: in_cut(cuts[1]), retakes: 0 },
        ],
        before,
        after: editor.project,
        words,
        transcribed: true,
        tick: None,
        whoosh: None,
    }
}

/// Frame `n` of the recap as RGBA, 1080 × 1920.
fn frame(path: &Path, n: u32) -> Vec<u8> {
    raw(path, &["-vf", &format!("select=eq(n\\,{n})"), "-frames:v", "1"])
}

fn pixel(rgba: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * 1080 + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

fn red([r, g, b]: [u8; 3]) -> bool {
    r > 200 && g < 110 && b < 110
}

/// Root mean square of the recap's sound from `a` to `b` seconds.
fn rms(path: &Path, a: f64, b: f64) -> f64 {
    let out = run(Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(path).args([
        "-ss",
        &a.to_string(),
        "-to",
        &b.to_string(),
        "-ac",
        "1",
        "-f",
        "f32le",
        "-",
    ]));
    let samples: Vec<f32> = out.stdout.as_chunks::<4>().0.iter().map(|&b| f32::from_le_bytes(b)).collect();
    (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64).sqrt()
}

#[test]
fn recap_marks_the_cuts_and_ends_on_the_finished_cut_as_the_renderer_draws_it() {
    if !available() {
        return;
    }
    let d = dir("recap");
    let recap = recap(&d);
    let out = d.join("recap.mp4");
    let options = RecapOptions { end_card: true, replace_existing: true };
    render(&recap, &d.join("cache"), &out, &options, &AtomicBool::new(false), |_| {}).unwrap();

    let j = info(&out);
    let streams = j["streams"].as_array().unwrap();
    let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
    assert_eq!((video["width"].as_u64(), video["height"].as_u64()), (Some(1080), Some(1920)));
    assert!(streams.iter().any(|s| s["codec_name"] == "aac"));
    let length = j["format"]["duration"].as_str().unwrap().parse::<f64>().unwrap();
    let expected = duration_us(&recap, true) as f64 / 1e6;
    assert!((length - expected).abs() < 0.05, "{length} s instead of {expected} s");
    assert!(length <= 30.0);

    // The opening bar is red where the recording was cut and not where it was kept.
    let [hook, fast, hold, ..] = sections(&recap, true);
    assert_eq!((hook, fast), (1_500_000, 5 * S));
    let opening = frame(&out, 15);
    let bar_x = |at_us: i64| (60.0 + 840.0 * at_us as f64 / (20 * S) as f64) as usize;
    assert!(red(pixel(&opening, bar_x(11 * S), 1046)), "{:?}", pixel(&opening, bar_x(11 * S), 1046));
    assert!(!red(pixel(&opening, bar_x(7 * S), 1046)), "{:?}", pixel(&opening, bar_x(7 * S), 1046));

    // The recording plays at 4×: 3 s into it is cut; 8 s is kept, on a caption page without cut words.
    let at = |recording_us: i64| ((hook + recording_us / 4) * 30 / S) as u32;
    let cut = frame(&out, at(3 * S));
    let kept = frame(&out, at(8 * S));
    assert!(red(pixel(&cut, 5, 960)), "the cut has a red edge: {:?}", pixel(&cut, 5, 960));
    assert!(!red(pixel(&kept, 5, 960)), "a kept moment has no red edge: {:?}", pixel(&kept, 5, 960));
    let struck = |rgba: &[u8]| {
        (1250..1400).flat_map(|y| (60..900).map(move |x| (x, y))).filter(|&(x, y)| red(pixel(rgba, x, y))).count()
    };
    assert!(struck(&cut) > 500, "the cut words are struck through: {} red pixels", struck(&cut));
    assert_eq!(struck(&kept), 0, "nothing kept is struck through");

    // Five seconds into the finished cut, past its label, the recap shows the renderer's frame.
    let start = hook + fast + hold;
    let n = ((start + 5 * S) * 30 / S) as u32;
    let into = nuzky_engine::export::frame_time_us(n as u64, 30) - start;
    let mut renderer = Renderer::new().unwrap();
    let drawn = renderer.render(&recap.after, into, 1080, 1920, Wait::Exact, false).unwrap();
    let shown = frame(&out, n);
    let error = mae(&drawn, &shown);
    assert!(error < 4.0, "the finished cut differs from the renderer by {error:.2} per channel");

    // The fast recording is silent and the finished cut plays its own sound.
    let (fast_rms, cut_rms) = (rms(&out, 2.0, 6.0), rms(&out, start as f64 / 1e6 + 1.0, start as f64 / 1e6 + 4.0));
    assert!(fast_rms < 0.001 && cut_rms > 0.1, "fast {fast_rms:.4}, finished cut {cut_rms:.4}");
    assert!(
        !std::fs::read_dir(&d).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".nuzky-part-"))
    );
}

#[test]
fn recap_keeps_an_existing_file_and_stops_when_cancelled() {
    if !available() {
        return;
    }
    let d = dir("recap-refuse");
    let recap = recap(&d);
    let out = d.join("existing.mp4");
    std::fs::write(&out, b"someone's video").unwrap();
    let options = RecapOptions { end_card: false, replace_existing: false };
    let error = render(&recap, &d.join("cache"), &out, &options, &AtomicBool::new(false), |_| {}).unwrap_err();
    assert!(error.to_string().contains("OUTPUT_EXISTS"), "{error:#}");
    assert_eq!(std::fs::read(&out).unwrap(), b"someone's video");

    let cancel = AtomicBool::new(false);
    let fresh = d.join("cancelled.mp4");
    let error = render(&recap, &d.join("cache"), &fresh, &options, &cancel, |p| {
        if p > 0.1 {
            cancel.store(true, Ordering::Relaxed);
        }
    })
    .unwrap_err();
    assert!(error.to_string().starts_with("CANCELLED"), "{error:#}");
    assert!(!fresh.exists());
    assert!(
        !std::fs::read_dir(&d).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".nuzky-part-"))
    );
}
