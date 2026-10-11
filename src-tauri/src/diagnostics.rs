//! What a person can send when Nuzky misbehaves: a log of what went wrong in the `logs` folder of Nuzky's data
//! folder, and a short diagnostics text with the version, system, graphics and the end of the log. Nothing is sent
//! anywhere; the person copies the text or opens the folder.
//!
//! The log keeps no one's work or identity. Text between double quotes or backticks, where captions and other typed
//! text appear in error and panic messages, is left out, a path keeps only its last part and tokens and keys are
//! hidden.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use log::{Log, Metadata, Record};
use tauri::AppHandle;

use crate::{CmdResult, store, updates};

const LOG: &str = "nuzky.log";
const OLDER_LOG: &str = "nuzky.1.log";
/// Each of the two files grows to this size; then the older one is replaced.
const MAX_BYTES: u64 = 1 << 20;
/// How much of the log Copy diagnostics includes.
const TAIL_LINES: usize = 200;
const RUNNING: &str = "running-";

pub fn logs_dir() -> PathBuf {
    store::data_dir().join("logs")
}

/// Logs to the terminal as before and, from the same records, to the log file; records panics; and notes whether
/// the previous Nuzky crashed.
pub fn init() {
    let stderr = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,wgpu_core=warn,wgpu_hal=warn,naga=warn"),
    )
    .build();
    let dir = logs_dir();
    let file = LogFile::open(&dir).inspect_err(|error| eprintln!("Nuzky cannot write its log: {error}")).ok();
    if let Err(error) = watch_for_crashes(&dir) {
        eprintln!("Nuzky cannot tell whether it crashed: {error}");
    }
    log::set_max_level(stderr.filter());
    if log::set_boxed_logger(Box::new(Logger { stderr, file: Mutex::new(file) })).is_ok() {
        record_panics();
    }
}

struct Logger {
    stderr: env_logger::Logger,
    file: Mutex<Option<LogFile>>,
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.stderr.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if !self.stderr.matches(record) {
            return;
        }
        self.stderr.log(record);
        if let Some(file) = self.file.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            let now = jiff::Timestamp::now();
            file.append(&format!("{now:.0} {} {}: {}\n", record.level(), record.target(), record.args()));
        }
    }

    fn flush(&self) {}
}

struct LogFile {
    dir: PathBuf,
    file: File,
    size: u64,
}

impl LogFile {
    fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let file = OpenOptions::new().create(true).append(true).open(dir.join(LOG))?;
        Ok(Self { dir: dir.into(), size: file.metadata()?.len(), file })
    }

    fn append(&mut self, record: &str) {
        let record = scrub(record);
        if self.size + record.len() as u64 > MAX_BYTES
            && std::fs::rename(self.dir.join(LOG), self.dir.join(OLDER_LOG)).is_ok()
            && let Ok(fresh) = Self::open(&self.dir)
        {
            *self = fresh;
        }
        if self.file.write_all(record.as_bytes()).is_ok() {
            self.size += record.len() as u64;
        }
    }
}

/// Panics caught by a job or the preview are logged here too, with where they happened.
fn record_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let place = info.location().map(|at| format!(" at {}:{}", at.file(), at.line())).unwrap_or_default();
        let message = info.payload_as_str().unwrap_or("no message");
        log::error!(target: "panic", "Thread {} panicked{place}: {message}", thread.name().unwrap_or("unnamed"));
        previous(info);
    }));
}

static CRASHED: AtomicBool = AtomicBool::new(false);

/// Whether a Nuzky that ran before this one ended without quitting: it crashed, hung and was killed, or the
/// computer lost power.
pub fn crashed_before() -> bool {
    CRASHED.load(Ordering::Relaxed)
}

/// The file this app holds locked while it runs and removes when it quits.
static MARKER: OnceLock<(PathBuf, File)> = OnceLock::new();

/// Every running Nuzky holds a lock on a file of its own in `dir`. A file nobody holds was left by one that never
/// quit; files another open Nuzky window holds stay.
fn watch_for_crashes(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(RUNNING) {
            continue;
        }
        let left = File::open(entry.path()).is_ok_and(|file| file.try_lock().is_ok());
        if left {
            CRASHED.store(true, Ordering::Relaxed);
            std::fs::remove_file(entry.path()).ok();
        }
    }
    let path = dir.join(format!("{RUNNING}{}", std::process::id()));
    let file = File::create(&path)?;
    file.lock()?;
    #[cfg(unix)]
    quit_on_signals(&path);
    MARKER.set((path, file)).ok();
    Ok(())
}

/// Quitting normally is not a crash.
pub fn quit() {
    if let Some((path, _)) = MARKER.get() {
        std::fs::remove_file(path).ok();
    }
}

/// Logging out, Ctrl+C in a terminal and `kill` end the app without a crash, so they remove its file too.
#[cfg(unix)]
fn quit_on_signals(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    static PATH: OnceLock<std::ffi::CString> = OnceLock::new();
    extern "C" fn on_signal(signal: libc::c_int) {
        // Only calls a signal handler may make: the signal then ends the app as it did before.
        unsafe {
            if let Some(path) = PATH.get() {
                libc::unlink(path.as_ptr());
            }
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
        }
    }
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return };
    if PATH.set(path).is_ok() {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            unsafe { libc::signal(signal, on_signal as *const () as libc::sighandler_t) };
        }
    }
}

static GRAPHICS: Mutex<Option<String>> = Mutex::new(None);

/// The GPU and graphics backend the preview draws with.
pub fn set_graphics(adapter: &str) {
    *GRAPHICS.lock().unwrap_or_else(PoisonError::into_inner) = Some(adapter.to_owned());
}

fn text(version: &str, dir: &Path) -> String {
    let graphics = GRAPHICS.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut lines: Vec<String> = [OLDER_LOG, LOG]
        .iter()
        .filter_map(|name| std::fs::read(dir.join(name)).ok())
        .flat_map(|bytes| String::from_utf8_lossy(&bytes).lines().map(String::from).collect::<Vec<_>>())
        .collect();
    lines.drain(..lines.len().saturating_sub(TAIL_LINES));
    let log = if lines.is_empty() { "The log is empty.".to_owned() } else { lines.join("\n") };
    format!(
        "Nuzky {version}\nSystem: {}\nGraphics: {}\n\nLast {} lines of the log:\n{log}\n",
        updates::system(),
        graphics.as_deref().unwrap_or("the preview has not started"),
        lines.len(),
    )
}

/// What Copy diagnostics puts on the clipboard. The log in it was already cleaned when written.
#[tauri::command]
pub fn diagnostics(app: AppHandle) -> String {
    text(&app.package_info().version.to_string(), &logs_dir())
}

#[tauri::command]
pub fn open_logs(app: AppHandle) -> CmdResult<()> {
    let dir = logs_dir();
    std::fs::create_dir_all(&dir).map_err(crate::err)?;
    updates::open(&app, &dir.to_string_lossy())
}

/// An error in the interface, which only the log hears about.
#[tauri::command]
pub fn log_ui_error(message: String) {
    let message: String = message.chars().take(4000).collect();
    log::error!(target: "ui", "{message}");
}

/// The log line without typed text, folders or secrets.
pub fn scrub(text: &str) -> String {
    hide_secrets(&shorten_paths(&hide_quoted(text)))
}

/// Text between double quotes or backticks on one line: captions, words and names in error and panic messages.
fn hide_quoted(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let close = (c == '"' || c == '`').then(|| closing(&chars, i + 1, c)).flatten();
        if let Some(end) = close {
            out.push(c);
            if end > i + 1 {
                out.push('…');
            }
            out.push(c);
            i = end + 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Where the quote opened before `from` closes on the same line, past escaped quotes.
fn closing(chars: &[char], from: usize, quote: char) -> Option<usize> {
    let mut i = from;
    while i < chars.len() && chars[i] != '\n' {
        match chars[i] {
            '\\' => i += 2,
            c if c == quote => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Paths lose their folders, which name the person and where their files are. A path may hold spaces, so it is
/// taken to run to the end of the clause; that can drop words after it, never a folder.
fn shorten_paths(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if !path_starts(&chars, i) {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut end = i;
        while end < chars.len() && !clause_ends(&chars, end) {
            end += 1;
        }
        let path = &chars[i..end];
        let trimmed = path.iter().rposition(|c| !is_separator(*c)).map_or(0, |last| last + 1);
        let name = path[..trimmed].iter().rposition(|c| is_separator(*c)).map_or(0, |sep| sep + 1);
        out.extend(&path[name..]);
        i = end;
    }
    out
}

fn is_separator(c: char) -> bool {
    c == '/' || c == '\\'
}

fn path_starts(chars: &[char], i: usize) -> bool {
    let after_gap = i == 0 || chars[i - 1].is_whitespace() || "\"'`([{<=,".contains(chars[i - 1]);
    let next = |n: usize| chars.get(i + n).copied();
    let rooted = is_separator(chars[i]) && next(1).is_some_and(|c| !c.is_whitespace());
    let home = chars[i] == '~' && next(1).is_some_and(is_separator);
    let drive = chars[i].is_ascii_alphabetic() && next(1) == Some(':') && next(2).is_some_and(is_separator);
    let file_url = chars[i..].starts_with(&['f', 'i', 'l', 'e', ':', '/', '/']);
    after_gap && (rooted || home || drive || file_url)
}

fn clause_ends(chars: &[char], i: usize) -> bool {
    "\n\r\"'`<>|;".contains(chars[i]) || ([':', ','].contains(&chars[i]) && chars.get(i + 1) == Some(&' '))
}

/// Tokens and keys in `name=value` and after `Bearer`, and long hexadecimal strings, which is what the preview, IPC
/// and test bridge secrets are.
fn hide_secrets(text: &str) -> String {
    const HIDDEN: &str = "[hidden]";
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let value_ends = |from: usize| {
            (from..chars.len())
                .find(|&j| chars[j].is_whitespace() || "&\"',:;)]}".contains(chars[j]))
                .unwrap_or(chars.len())
        };
        if chars[i] == '=' {
            let name: String = out.chars().rev().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            let name = name.chars().rev().collect::<String>().to_ascii_lowercase();
            if ["token", "secret", "key", "password", "auth"].iter().any(|secret| name.ends_with(secret)) {
                out.push('=');
                out.push_str(HIDDEN);
                i = value_ends(i + 1);
                continue;
            }
        }
        if chars[i..].starts_with(&['B', 'e', 'a', 'r', 'e', 'r', ' ']) {
            out.push_str("Bearer ");
            out.push_str(HIDDEN);
            i = value_ends(i + 7);
            continue;
        }
        let word_starts = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        let hex = chars[i..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
        if word_starts && hex >= 32 && chars.get(i + hex).is_none_or(|c| !c.is_ascii_alphanumeric()) {
            out.push_str(HIDDEN);
            i += hex;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_keeps_file_names_and_drops_folders_text_and_secrets() {
        for (line, clean) in [
            (
                "Cannot read /home/jana/Videos/Trip 2024/clip 1.mov: No such file",
                "Cannot read clip 1.mov: No such file",
            ),
            ("ffmpeg: '/Users/jana/Movies/IMG_0042.MOV': Invalid data", "ffmpeg: 'IMG_0042.MOV': Invalid data"),
            (r"Cannot open C:\Users\Jana\Videos\clip.mp4, skipping", "Cannot open clip.mp4, skipping"),
            ("Saved ~/Projects/wedding.nuzky", "Saved wedding.nuzky"),
            ("open file:///home/jana/a.mov", "open a.mov"),
            ("Caption \"Ahoj, tady Jana\" is too long", "Caption \"…\" is too long"),
            (
                "byte index 2 is not a char boundary; it is inside '€' (bytes 1..4) of `#€ secret plan`",
                "byte index 2 is not a char boundary; it is inside '€' (bytes 1..4) of `…`",
            ),
            ("left: \"say \\\"hi\\\" now\"", "left: \"…\""),
            (
                "Requesting https://freesound.org/apiv2/search/?query=rain&token=abcDEF123: 401",
                "Requesting https://freesound.org/apiv2/search/?query=rain&token=[hidden]: 401",
            ),
            ("Authorization: Bearer sk-abc.def", "Authorization: Bearer [hidden]"),
            (
                "ws://127.0.0.1:4321/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef refused",
                "ws://127.0.0.1:4321/[hidden] refused",
            ),
            (
                "Preview renderer on Apple M2 Pro (Metal), 9/16 canvas, video/mp4",
                "Preview renderer on Apple M2 Pro (Metal), 9/16 canvas, video/mp4",
            ),
            ("https://huggingface.co/ggml-org/whisper-vad", "https://huggingface.co/ggml-org/whisper-vad"),
        ] {
            assert_eq!(scrub(line), clean, "{line}");
        }
    }

    /// The log a panic leaves: one line with where it happened, no token and no folder of the user's.
    #[test]
    fn a_panic_is_logged_without_tokens_or_folders() {
        let dir = std::env::temp_dir().join(format!("nuzky-panic-log-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let file = LogFile::open(&dir).unwrap();
        let logger = Logger {
            stderr: env_logger::Builder::new().filter_level(log::LevelFilter::Info).is_test(true).build(),
            file: Mutex::new(Some(file)),
        };
        // The test binary has one logger; this one writes into `dir` for as long as the test needs it.
        assert!(log::set_boxed_logger(Box::new(logger)).is_ok(), "another test installed a logger");
        log::set_max_level(log::LevelFilter::Info);
        record_panics();
        // Shaped like the preview and IPC secrets: 64 hexadecimal digits.
        let token = "5eed".repeat(16);
        let panicked = std::thread::Builder::new()
            .name("export".into())
            .spawn({
                let token = token.clone();
                move || panic!("Cannot write /home/jana/Videos/private/reel.mp4 with token={token}")
            })
            .unwrap()
            .join();
        let _ = std::panic::take_hook();
        assert!(panicked.is_err());
        let log = std::fs::read_to_string(dir.join(LOG)).unwrap();
        // Other tests may panic meanwhile; their lines land here too.
        let line =
            log.lines().find(|line| line.contains("Thread export")).unwrap_or_else(|| panic!("no panic in {log:?}"));
        assert!(line.contains(" ERROR panic: Thread export panicked at "), "{line}");
        assert!(line.contains("diagnostics.rs:"), "{line}");
        assert!(line.ends_with("Cannot write reel.mp4 with token=[hidden]"), "{line}");
        assert!(!log.contains(&token) && !log.contains("/home/jana") && !log.contains("private"), "{log}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn diagnostics_end_with_the_newest_lines_of_both_logs() {
        let dir = std::env::temp_dir().join(format!("nuzky-diagnostics-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OLDER_LOG), (0..150).map(|n| format!("old {n}\n")).collect::<String>()).unwrap();
        std::fs::write(dir.join(LOG), (0..100).map(|n| format!("new {n}\n")).collect::<String>()).unwrap();
        let text = text("0.1.0-alpha", &dir);
        assert!(text.starts_with("Nuzky 0.1.0-alpha\nSystem: "), "{text}");
        assert!(
            text.contains(&format!(" · {}\nGraphics: the preview has not started\n", std::env::consts::ARCH)),
            "{text}"
        );
        assert!(text.contains("\n\nLast 200 lines of the log:\nold 50\n"), "{text}");
        assert!(text.contains("\nold 149\nnew 0\n") && text.ends_with("\nnew 99\n"), "{text}");
        assert!(!text.contains("old 49\n"), "{text}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_full_log_moves_aside_and_the_next_record_starts_a_new_one() {
        let dir = std::env::temp_dir().join(format!("nuzky-rotate-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LOG), vec![b'x'; MAX_BYTES as usize - 10]).unwrap();
        let mut file = LogFile::open(&dir).unwrap();
        file.append("a record longer than ten bytes\n");
        assert_eq!(std::fs::metadata(dir.join(OLDER_LOG)).unwrap().len(), MAX_BYTES - 10);
        assert_eq!(std::fs::read_to_string(dir.join(LOG)).unwrap(), "a record longer than ten bytes\n");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_marker_nobody_holds_means_the_last_run_crashed() {
        let dir = std::env::temp_dir().join(format!("nuzky-crash-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        // Another Nuzky that is still open holds its file; one that crashed left its file unheld.
        let open = File::create(dir.join(format!("{RUNNING}1"))).unwrap();
        open.lock().unwrap();
        File::create(dir.join(format!("{RUNNING}2"))).unwrap();
        watch_for_crashes(&dir).unwrap();
        assert!(crashed_before());
        assert!(dir.join(format!("{RUNNING}1")).exists(), "the open window's file stays");
        assert!(!dir.join(format!("{RUNNING}2")).exists(), "the crashed one's file is gone");
        let own = dir.join(format!("{RUNNING}{}", std::process::id()));
        assert!(own.exists());
        quit();
        assert!(!own.exists(), "quitting is not a crash");
        drop(open);
        std::fs::remove_dir_all(dir).ok();
    }
}
