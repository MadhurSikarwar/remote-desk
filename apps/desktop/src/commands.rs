//! Commands the UI may invoke. They only forward to the controllers, which
//! hold the logic and its tests.

use rc_core::Config;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::share::{DisplaySummary, ShareController, ShareSettings, ShareStatus};
use crate::state::{AppState, Status};
use crate::viewer::ViewerController;

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
pub fn list_displays(share: State<'_, ShareController>) -> Result<Vec<DisplaySummary>, String> {
    share.displays()
}

// Commands that can wait on a socket or a thread are `async`, which runs
// them off the UI thread so the window stays responsive.

/// Only ever called from the Start button: sharing never begins on its own.
#[tauri::command(async)]
pub fn start_share(
    state: State<'_, AppState>,
    share: State<'_, ShareController>,
    display: usize,
) -> Result<ShareStatus, String> {
    let config = state.config();
    let status = share.start(ShareSettings {
        display,
        port: config.network.direct_port,
        max_fps: config.video.max_fps,
        bitrate_kbps: config.video.max_bitrate_kbps,
    })?;
    // `display` cannot be used as a field name: the macro shadows it.
    let index = display;
    tracing::info!(
        index,
        port = status.port,
        "screen sharing started by the user"
    );
    Ok(status)
}

#[tauri::command(async)]
pub fn stop_share(share: State<'_, ShareController>) -> ShareStatus {
    let status = share.stop();
    tracing::info!(frames = status.frames, "screen sharing stopped");
    status
}

#[tauri::command]
pub fn share_status(share: State<'_, ShareController>) -> ShareStatus {
    share.status()
}

/// Connects to a host on this computer; video packets arrive on `packets`.
#[tauri::command(async)]
pub fn view_connect(
    viewer: State<'_, ViewerController>,
    port: u16,
    packets: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    viewer.connect(port, move |packet| {
        // Fails only once the window is gone; the session ends with it.
        let _ = packets.send(InvokeResponseBody::Raw(packet));
    })
}

#[tauri::command]
pub fn view_request_keyframe(viewer: State<'_, ViewerController>) -> Result<(), String> {
    viewer.request_keyframe()
}

#[tauri::command(async)]
pub fn view_disconnect(viewer: State<'_, ViewerController>) {
    viewer.disconnect();
}
