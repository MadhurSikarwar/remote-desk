//! Commands the UI may invoke. They only forward to `AppState`, which holds
//! the logic and its tests.

use rc_core::Config;
use tauri::State;

use crate::capture::{CaptureController, CaptureStatus, DisplaySummary};
use crate::state::{AppState, Status};

#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> Status {
    state.status()
}

#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Config {
    state.config()
}

#[tauri::command]
pub fn save_config(state: State<'_, AppState>, config: Config) -> Result<Config, String> {
    state.update(config)
}

#[tauri::command]
pub fn list_displays(capture: State<'_, CaptureController>) -> Result<Vec<DisplaySummary>, String> {
    capture.displays()
}

/// Only ever called from the Start button: capture never begins on its own.
#[tauri::command]
pub fn start_capture(
    state: State<'_, AppState>,
    capture: State<'_, CaptureController>,
    display: usize,
) -> Result<CaptureStatus, String> {
    let status = capture.start(display, state.config().video.max_fps)?;
    // `display` cannot be used as a field name: the macro shadows it.
    let index = display;
    tracing::info!(index, "screen capture started by the user");
    Ok(status)
}

#[tauri::command]
pub fn stop_capture(capture: State<'_, CaptureController>) -> CaptureStatus {
    let status = capture.stop();
    tracing::info!(frames = status.frames, "screen capture stopped");
    status
}

#[tauri::command]
pub fn capture_status(capture: State<'_, CaptureController>) -> CaptureStatus {
    capture.status()
}
