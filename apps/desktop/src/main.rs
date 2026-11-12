#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capture;
mod commands;
mod state;

use tauri::{Manager, RunEvent};

fn main() {
    let (state, mut log_guard) = state::AppState::start();

    let app = tauri::Builder::default()
        .manage(state)
        .manage(capture::CaptureController::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_config,
            commands::save_config,
            commands::list_displays,
            commands::start_capture,
            commands::stop_capture,
            commands::capture_status
        ])
        .build(tauri::generate_context!())
        .expect("failed to start the application");

    app.run(move |handle, event| {
        if let RunEvent::Exit = event {
            handle.state::<capture::CaptureController>().stop();
            tracing::info!("application exiting");
            // Dropping the guard flushes the log file; the process exits
            // without running destructors after this callback returns.
            log_guard.take();
        }
    });
}
