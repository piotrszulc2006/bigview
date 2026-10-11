#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};
use std::time::Instant;

use bigview_core::{BigFile, DEFAULT_STRIDE};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

/// Longest line prefix sent to the UI. A single multi-megabyte line (minified
/// JSON, a binary blob) would otherwise freeze the webview's text layout.
const MAX_LINE_BYTES: usize = 4096;
/// Upper bound on one `get_lines` request, whatever the frontend asks for.
const MAX_WINDOW: usize = 2000;

#[derive(Default)]
struct AppState {
    file: Mutex<Option<Arc<BigFile>>>,
}

impl AppState {
    fn current(&self) -> Result<Arc<BigFile>, String> {
        self.file.lock().unwrap().clone().ok_or_else(|| "no file is open".to_string())
    }
}

#[derive(Serialize, Clone)]
struct Progress {
    done: u64,
    total: u64,
}

#[derive(Serialize)]
struct FileInfo {
    path: String,
    bytes: u64,
    lines: u64,
    index_ms: u64,
}

#[derive(Serialize)]
struct LineDto {
    text: String,
    truncated: bool,
}

/// Path given on the command line, so `bigview some.log` opens it directly.
#[tauri::command]
fn initial_path() -> Option<String> {
    std::env::args().nth(1)
}

#[tauri::command]
async fn open_file(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<FileInfo, String> {
    let started = Instant::now();
    let to_open = path.clone();
    // Indexing scans the whole file; keep it off the async runtime's threads.
    let file = tauri::async_runtime::spawn_blocking(move || {
        BigFile::open_with(&to_open, DEFAULT_STRIDE, |done, total| {
            let _ = app.emit("index-progress", Progress { done, total });
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{path}: {e}"))?;

    let info = FileInfo {
        path,
        bytes: file.len_bytes(),
        lines: file.line_count(),
        index_ms: started.elapsed().as_millis() as u64,
    };
    *state.file.lock().unwrap() = Some(Arc::new(file));
    Ok(info)
}

#[tauri::command]
fn get_lines(state: State<'_, AppState>, start: u64, count: usize) -> Result<Vec<LineDto>, String> {
    let file = state.current()?;
    Ok(file
        .lines(start, count.min(MAX_WINDOW), MAX_LINE_BYTES)
        .into_iter()
        .map(|l| LineDto { text: l.text, truncated: l.truncated })
        .collect())
}

#[tauri::command]
async fn find_next(
    state: State<'_, AppState>,
    query: String,
    from_line: u64,
) -> Result<Option<u64>, String> {
    let file = state.current()?;
    // A miss scans to the end of the file, which can take seconds on many GB.
    tauri::async_runtime::spawn_blocking(move || file.find_next(query.as_bytes(), from_line))
        .await
        .map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![initial_path, open_file, get_lines, find_next])
        .run(tauri::generate_context!())
        .expect("error while running bigview");
}
