//! What a person can send when Nuzky misbehaves: a log of what went wrong in the `logs` folder of Nuzky's data
//! folder, and a short diagnostics text with the version, system, graphics and the end of the log. Nothing is sent
//! anywhere; the person copies the text or opens the folder.
//!
//! The log keeps no one's work or identity. Text between double quotes, curly quotes or backticks, where captions,
//! project names and other typed text appear in error and panic messages, is left out, a path keeps only its last
//! part and tokens and keys are hidden. Debug and trace records, where libraries dump whole HTTP requests, stay in
//! the terminal.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use log::{Level, Log, Metadata, Record};
use tauri::AppHandle;

use crate::{CmdResult, store, updates};

const LOG: &str = "nuzky.log";
const OLDER_LOG: &str = "nuzky.1.log";
/// Each of the two files grows to this size; then the older one is replaced.
const MAX_BYTES: u64 = 1 << 20;
/// How much of the log Copy diagnostics includes.
const TAIL_LINES: usize = 200;
const RUNNING: &str = "running-";
/// A marker while it is being locked, before it is published under `RUNNING`.
const STARTING: &str = "starting-";

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
    if log::set_boxed_logger(Box::new(Logger { stderr, file })).is_ok() {
        record_panics();
    }
}

struct Logger {
    stderr: env_logger::Logger,
    file: Option<LogFile>,
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
        if let Some(file) = &self.file
            && record.level() <= Level::Info
        {
            let now = jiff::Timestamp::now();
            file.append(&format!("{now:.0} {} {}: {}\n", record.level(), record.target(), record.args()));
        }
    }

    fn flush(&self) {}
}

/// Opened for each record, so a Nuzky in another window that moved the full log aside never leaves this one writing
/// into the old file.
struct LogFile {
    dir: PathBuf,
}

impl LogFile {
    fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self { dir: dir.into() })
    }

    fn append(&self, record: &str) {
        let record = scrub(record);
        let path = self.dir.join(LOG);
        if std::fs::metadata(&path).is_ok_and(|log| log.len() + record.len() as u64 > MAX_BYTES) {
            std::fs::rename(&path, self.dir.join(OLDER_LOG)).ok();
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
            file.write_all(record.as_bytes()).ok();
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
/// quit; files another open Nuzky window holds stay. A file shows under `RUNNING` only once it is locked, so a Nuzky
/// starting at the same moment never takes it for a crash.
fn watch_for_crashes(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(RUNNING) && !name.starts_with(STARTING) {
            continue;
        }
        let left = File::open(entry.path()).is_ok_and(|file| file.try_lock().is_ok());
        if left {
            CRASHED.fetch_or(name.starts_with(RUNNING), Ordering::Relaxed);
            std::fs::remove_file(entry.path()).ok();
        }
    }
    let starting = dir.join(format!("{STARTING}{}", std::process::id()));
    let file = File::create(&starting)?;
    file.lock()?;
    let path = dir.join(format!("{RUNNING}{}", std::process::id()));
    std::fs::rename(&starting, &path)?;
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

/// Text between double quotes, curly quotes or backticks on one line: captions, words and project names in error and
/// panic messages.
fn hide_quoted(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let quote = match c {
            '"' | '`' => Some(c),
            '“' => Some('”'),
            _ => None,
        };
        if let Some(end) = quote.and_then(|quote| closing(&chars, i + 1, quote)) {
            out.push(c);
            if end > i + 1 {
                out.push('…');
            }
            out.push(chars[end]);
            i = end + 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Where `quote` closes the quote opened before `from` on the same line, past escaped quotes.
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

/// Paths lose their folders, which name the person and where their files are. A folder may hold spaces, commas and
/// apostrophes, so a path is taken to run to the end of its line, or to the line's last `'` when one opened it; that
/// can drop words after it, never a folder.
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
        let line_end = (i..chars.len()).find(|&j| chars[j] == '\n' || chars[j] == '\r').unwrap_or(chars.len());
        let quoted = i > 0 && chars[i - 1] == '\'';
        let end = (i..line_end).rev().find(|&j| quoted && chars[j] == '\'').unwrap_or(line_end);
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

/// Tokens and keys in `name=value`, after `Bearer` and in an `Authorization` header, and long hexadecimal strings,
/// which is what the preview, IPC and test bridge secrets are.
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
        if out.ends_with("Authorization:") {
            out.push(' ');
            out.push_str(HIDDEN);
            i = (i..chars.len()).find(|&j| chars[j] == '\n').unwrap_or(chars.len());
            continue;
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
            ("Bearer sk-abc.def refused", "Bearer [hidden] refused"),
            ("Authorization: Token abc DEF\nnext", "Authorization: [hidden]\nnext"),
            ("Cannot open /Users/jana/Jana's private/clip.mov", "Cannot open clip.mov"),
            ("Cannot read /home/jana/Family, Jana/clip.mov: denied", "Cannot read clip.mov: denied"),
            ("ffmpeg: '/Users/jana/Jana's Videos/a.mov': Invalid data", "ffmpeg: 'a.mov': Invalid data"),
            ("No poster for “Klient Novák, svatba”: no GPU", "No poster for “…”: no GPU"),
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
            stderr: env_logger::Builder::new().filter_level(log::LevelFilter::Trace).is_test(true).build(),
            file: Some(file),
        };
        // The test binary has one logger; this one writes into `dir` for as long as the test needs it.
        assert!(log::set_boxed_logger(Box::new(logger)).is_ok(), "another test installed a logger");
        log::set_max_level(log::LevelFilter::Trace);
        record_panics();
        // How an HTTP library dumps a request it sends, which only the terminal may show.
        log::trace!(target: "ureq_proto::util", "Authorization: Token 1d2e3f4a5b6c");
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
        assert!(!log.contains("1d2e3f4a5b6c") && !log.contains("ureq_proto"), "{log}");
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
        let file = LogFile::open(&dir).unwrap();
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
