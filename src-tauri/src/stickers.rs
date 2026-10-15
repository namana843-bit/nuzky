use nuzky_mcp::stickers::{self, StickerCatalog};
use nuzky_session::Expect;
use tauri::{AppHandle, Manager};

use crate::{AppState, CmdResult, Snapshot, err};

/// Async, so parsing the catalogue the first time never holds up the window.
#[tauri::command]
pub async fn sticker_catalog() -> &'static StickerCatalog {
    stickers::catalog()
}

#[tauri::command]
pub async fn sticker_search(query: String) -> Vec<&'static str> {
    stickers::search(&query, 200)
}

#[tauri::command]
pub async fn sticker_thumbs(keys: Vec<String>, px: u32) -> CmdResult<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || stickers::thumbnails(&keys, px)).await.map_err(err)
}

#[tauri::command]
pub async fn sticker_add(
    app: AppHandle,
    key: String,
    start_us: i64,
    expected_epoch: Option<String>,
) -> CmdResult<Snapshot> {
    let state = app.state::<AppState>();
    let assets = state.project()?.assets.clone();
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        stickers::prepare(stickers::find(&key)?, &assets, start_us.max(0), None, None)
    })
    .await
    .map_err(err)?
    .map_err(err)?;
    let snap = state.apply_batch(prepared.edits, None, Expect::default(), expected_epoch.as_deref())?;
    crate::jobs::prepare_media(&state, &snap.project);
    Ok(snap)
}
