//! Commands the UI may invoke. They only forward to `AppState`, which holds
//! the logic and its tests.

use rc_core::Config;
use tauri::State;

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
