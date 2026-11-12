#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use tauri::RunEvent;

fn main() {
    let (state, mut log_guard) = state::AppState::start();

    let app = tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_config,
            commands::save_config
        ])
        .build(tauri::generate_context!())
        .expect("failed to start the application");

    app.run(move |_handle, event| {
        if let RunEvent::Exit = event {
            tracing::info!("application exiting");
            // Dropping the guard flushes the log file; the process exits
            // without running destructors after this callback returns.
            log_guard.take();
        }
    });
}
