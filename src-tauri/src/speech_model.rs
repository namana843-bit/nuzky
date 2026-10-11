//! When an agent's transcription needs the speech model and it is not installed, the app asks the user whether to
//! download it and downloads it as the Transcript panel does. Jobs that need it meanwhile wait for the same answer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use nuzky_engine::edit::new_id;
use nuzky_mcp::transcript;
use nuzky_session::jobs::{Progress, check_cancel};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::jobs::{Reporter, download_speech, missing_speech_mb, register, unregister};

/// What the dialog asks, sent as `speech-model-question`; null closes the dialog.
#[cfg_attr(test, derive(ts_rs::TS))]
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SpeechModelQuestion {
    pub id: String,
    /// What the download adds up to.
    pub size_mb: u32,
}

/// One question to the user and the download that follows it.
struct Round {
    question: SpeechModelQuestion,
    /// Jobs waiting for the answer; when the last one stops before the user answered, the question closes.
    waiting: usize,
    /// How far the download has come, once the user chose Download.
    downloaded: Option<f32>,
    /// How it ended, for every job that waited on it.
    outcome: Arc<OnceLock<Result<(), String>>>,
}

static ROUND: Mutex<Option<Round>> = Mutex::new(None);

const WITHOUT_IT: &str = "Do not call transcribe again in this conversation. Tell the user what you can do without a \
     transcript, such as cutting the long silences analyze kind silences finds, and that the Transcript tab downloads \
     the model whenever they want.";

/// Blocks an agent's transcription job until the speech model is installed, asking the user first.
pub fn ask(app: &AppHandle, cancel: &AtomicBool, progress: &Progress) -> anyhow::Result<()> {
    let outcome = {
        let mut round = ROUND.lock().unwrap();
        if let Some(round) = round.as_mut() {
            round.waiting += 1;
            round.outcome.clone()
        } else {
            let model = transcript::best_model();
            // Installed meanwhile, by another agent's answer or a panel.
            if transcript::models(model).is_ok() {
                return Ok(());
            }
            let question = SpeechModelQuestion { id: new_id(), size_mb: missing_speech_mb(model) };
            app.emit("speech-model-question", &question).ok();
            let outcome = Arc::default();
            *round = Some(Round { question, waiting: 1, downloaded: None, outcome: Arc::clone(&outcome) });
            outcome
        }
    };
    loop {
        if let Some(result) = outcome.get() {
            return result.clone().map_err(anyhow::Error::msg);
        }
        let downloaded = {
            let mut round = ROUND.lock().unwrap();
            let ours = round.as_mut().filter(|round| Arc::ptr_eq(&round.outcome, &outcome));
            if cancel.load(Ordering::Relaxed) {
                // A download the user started goes on; a question nobody needs the answer to goes away.
                let unneeded = ours.is_some_and(|ours| {
                    ours.waiting -= 1;
                    ours.waiting == 0 && ours.downloaded.is_none()
                });
                if unneeded && let Some(ours) = round.take() {
                    let _ = ours.outcome.set(Err("CANCELLED: job cancelled".into()));
                    app.emit("speech-model-question", None::<SpeechModelQuestion>).ok();
                }
                return check_cancel(cancel);
            }
            ours.and_then(|ours| ours.downloaded)
        };
        // The download's own job shows its percentage; this one's would make remaining_s the download's alone.
        progress.set(if downloaded.is_some() { "downloading_speech_model" } else { "waiting_for_user" }, None);
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The user's answer to question `id`: Download starts the download as a job with Stop; Not now tells the agents.
#[tauri::command]
pub fn answer_speech_model(app: AppHandle, id: String, download: bool) -> Result<(), String> {
    let mut round = ROUND.lock().unwrap();
    // Answered already, or no agent waits for it any more.
    let Some(current) = round.as_mut().filter(|round| round.question.id == id && round.downloaded.is_none()) else {
        return Ok(());
    };
    app.emit("speech-model-question", None::<SpeechModelQuestion>).ok();
    if !download {
        let _ = current
            .outcome
            .set(Err(format!("MODEL_DECLINED: the user chose not to download the speech model now. {WITHOUT_IT}")));
        *round = None;
        return Ok(());
    }
    let job_id = format!("speech-model:{id}");
    let cancel = register(&app, &job_id).ok_or("The speech model is already downloading")?;
    current.downloaded = Some(0.0);
    drop(round);
    let (worker_app, worker_job) = (app.clone(), job_id.clone());
    let spawned = std::thread::Builder::new().name("speech-model".into()).spawn(move || {
        let mut rep = Reporter::new(&worker_app, &worker_job, "speech-model", "Speech model".into());
        let result = download_speech(transcript::best_model(), &cancel, |done, phase| {
            rep.progress(done, Some(phase));
            if let Some(round) = ROUND.lock().unwrap().as_mut().filter(|round| round.question.id == id) {
                round.downloaded = Some(done);
            }
        });
        let stopped = cancel.load(Ordering::Relaxed);
        let outcome = match &result {
            Ok(_) => Ok(()),
            Err(_) if stopped => {
                Err(format!("MODEL_DECLINED: the user stopped downloading the speech model. {WITHOUT_IT}"))
            }
            Err(error) => {
                Err(format!("MODEL_MISSING: the speech model could not be downloaded ({error:#}). {WITHOUT_IT}"))
            }
        };
        rep.finish(result.map(|_| None), stopped);
        unregister(&worker_app, &worker_job);
        if let Some(round) = ROUND.lock().unwrap().take_if(|round| round.question.id == id) {
            let _ = round.outcome.set(outcome);
        }
    });
    if let Err(error) = spawned {
        unregister(&app, &job_id);
        let message = format!("Starting the download: {error}");
        if let Some(round) = ROUND.lock().unwrap().take() {
            let _ = round.outcome.set(Err(format!("MODEL_MISSING: {message}. {WITHOUT_IT}")));
        }
        return Err(message);
    }
    Ok(())
}
