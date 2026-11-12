#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod share;
mod state;
mod viewer;

use tauri::{Manager, RunEvent};

fn main() {
    let (state, mut log_guard) = state::AppState::start();

    let app = tauri::Builder::default()
        .manage(state)
        .manage(share::ShareController::default())
        .manage(viewer::ViewerController::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_config,
            commands::save_config,
            commands::list_displays,
            commands::start_share,
            commands::stop_share,
            commands::share_status,
            commands::view_connect,
            commands::view_request_keyframe,
            commands::view_disconnect
        ])
        .build(tauri::generate_context!())
        .expect("failed to start the application");

    app.run(move |handle, event| {
        if let RunEvent::Exit = event {
            handle.state::<viewer::ViewerController>().disconnect();
            handle.state::<share::ShareController>().stop();
            tracing::info!("application exiting");
            // Dropping the guard flushes the log file; the process exits
            // without running destructors after this callback returns.
            log_guard.take();
        }
    });
}
