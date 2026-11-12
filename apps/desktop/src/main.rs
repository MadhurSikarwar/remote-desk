#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod share;
mod state;
mod viewer;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

/// How long shutdown may take before the process is ended by force.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(5);

/// Ends screen sharing and viewing. Called as soon as the window goes away,
/// not only at the very end of shutdown: the application must never keep
/// sharing a screen without a window that shows it. Safe to call repeatedly.
fn stop_sessions(app: &AppHandle) {
    tracing::debug!("stopping sessions");
    // On the UI thread, so the viewer thread is not waited for.
    app.state::<viewer::ViewerController>().abandon();
    tracing::debug!("viewer stopped");
    let status = app.state::<share::ShareController>().stop();
    if status.port.is_some() {
        tracing::info!(frames = status.frames, "screen sharing stopped at shutdown");
    }
    tracing::debug!("sessions stopped");
}

fn main() {
    let (state, log_guard) = state::AppState::start();
    // Shared with the shutdown watchdog, which must be able to flush the log.
    let log_guard = Arc::new(Mutex::new(log_guard));
    let flush_log = move || drop(share::lock(&log_guard).take());

    // Shutdown has been seen to stall. A stalled process would linger with
    // no window, so once the window is gone it gets a deadline.
    let watchdog_started = Arc::new(AtomicBool::new(false));
    let start_watchdog = {
        let flush_log = flush_log.clone();
        move || {
            if watchdog_started.swap(true, Ordering::Relaxed) {
                return;
            }
            let flush_log = flush_log.clone();
            thread::spawn(move || {
                thread::sleep(SHUTDOWN_LIMIT);
                tracing::error!("shutdown did not finish in time; ending the process");
                flush_log();
                std::process::exit(1);
            });
        }
    };

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
            commands::set_share_input,
            commands::view_connect,
            commands::view_request_keyframe,
            commands::view_disconnect,
            commands::view_send_input
        ])
        .on_window_event({
            let start_watchdog = start_watchdog.clone();
            move |window, event| match event {
                WindowEvent::CloseRequested { .. } => {
                    tracing::info!(window = window.label(), "window close requested");
                }
                WindowEvent::Destroyed => {
                    tracing::info!(window = window.label(), "window destroyed");
                    start_watchdog();
                    stop_sessions(window.app_handle());
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to start the application");

    app.run(move |handle, event| match event {
        RunEvent::ExitRequested { .. } => {
            tracing::info!("exit requested");
            start_watchdog();
            stop_sessions(handle);
        }
        RunEvent::Exit => {
            stop_sessions(handle);
            tracing::info!("application exiting");
            // Dropping the guard flushes the log file; the process exits
            // without running destructors after this callback returns.
            flush_log();
        }
        _ => {}
    });
}
