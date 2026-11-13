//! Commands the UI may invoke. They only forward to the controllers, which
//! hold the logic and its tests.

use std::time::Duration;

use rc_core::Config;
use rc_crypto::{fingerprint, from_hex, to_hex};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::share::{DisplaySummary, ShareController, ShareSettings, ShareStatus};
use crate::state::{AppState, Status};
use crate::viewer::{HostInfo, ViewerController, ViewerInput};

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
        allow_input: config.security.allow_input,
        allow_remote: config.network.allow_remote_connections,
        lan_discovery: config.network.lan_discovery,
        device_name: config.device.name,
        approval_timeout: Duration::from_secs(u64::from(config.security.approval_timeout_secs)),
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

/// The user's answer to a viewer asking to see the screen. Only ever called
/// from the Allow and Deny buttons.
#[tauri::command]
pub fn answer_approval(
    share: State<'_, ShareController>,
    id: u64,
    allow: bool,
    remember: bool,
) -> Result<ShareStatus, String> {
    let status = share.answer(id, allow, remember)?;
    tracing::info!(id, allow, remember, "viewer request answered by the user");
    Ok(status)
}

/// The host's own switch for letting the viewer control this computer.
#[tauri::command]
pub fn set_share_input(share: State<'_, ShareController>, allowed: bool) -> ShareStatus {
    let status = share.set_input_allowed(allowed);
    tracing::info!(allowed, "viewer control switched by the user");
    status
}

#[tauri::command]
pub fn share_status(share: State<'_, ShareController>) -> ShareStatus {
    share.status()
}

/// Connects to a host and returns who answered. Packets arrive on `packets`
/// once the host allows the session. `expected_key` (hex) comes from a
/// discovery result; the connection is dropped if the host holds another.
#[tauri::command(async)]
pub fn view_connect(
    state: State<'_, AppState>,
    viewer: State<'_, ViewerController>,
    address: String,
    expected_key: Option<String>,
    packets: Channel<InvokeResponseBody>,
) -> Result<HostInfo, String> {
    let expected = match expected_key {
        Some(hex) => Some(from_hex(&hex).ok_or("The device key is not valid")?),
        None => None,
    };
    viewer.connect(
        &address,
        expected,
        &state.config().device.name,
        move |packet| {
            // Fails only once the window is gone; the session ends with it.
            let _ = packets.send(InvokeResponseBody::Raw(packet));
        },
    )
}

/// A device that answered a discovery query.
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredDevice {
    /// What it calls itself. Not verified.
    pub name: String,
    /// `ip:port` to connect to.
    pub address: String,
    pub fingerprint: String,
    /// The key it announced, hex; passed back to `view_connect`.
    pub public_key: String,
    /// This is the device the question came from.
    pub this_device: bool,
}

/// Asks the local network which devices are sharing. Takes about two
/// seconds.
#[tauri::command(async)]
pub fn discover_devices(state: State<'_, AppState>) -> Result<Vec<DiscoveredDevice>, String> {
    let own = state.security().map(|s| *s.identity().public());
    let found = rc_net::scan(Duration::from_millis(1500)).map_err(|e| e.to_string())?;
    Ok(found
        .into_iter()
        .map(|f| DiscoveredDevice {
            name: crate::secure::display_name(&f.announcement.name),
            address: std::net::SocketAddr::new(f.address, f.announcement.port).to_string(),
            fingerprint: fingerprint(&f.announcement.public_key),
            public_key: to_hex(&f.announcement.public_key),
            this_device: own == Some(f.announcement.public_key),
        })
        .collect())
}

#[tauri::command]
pub fn view_request_keyframe(viewer: State<'_, ViewerController>) -> Result<(), String> {
    viewer.request_keyframe()
}

#[tauri::command(async)]
pub fn view_disconnect(viewer: State<'_, ViewerController>) {
    viewer.disconnect();
}

#[tauri::command]
pub fn view_send_input(
    viewer: State<'_, ViewerController>,
    input: ViewerInput,
) -> Result<(), String> {
    viewer.send_input(&input)
}

/// A device the user approved and chose to remember.
#[derive(Debug, Clone, Serialize)]
pub struct RememberedDevice {
    pub name: String,
    pub fingerprint: String,
    pub approved_at: u64,
}

#[tauri::command]
pub fn list_remembered(state: State<'_, AppState>) -> Vec<RememberedDevice> {
    let Some(security) = state.security() else {
        return Vec::new();
    };
    let trust = security.trust();
    trust
        .devices()
        .iter()
        .filter_map(|device| {
            Some(RememberedDevice {
                name: device.name.clone(),
                fingerprint: device.fingerprint()?,
                approved_at: device.approved_at,
            })
        })
        .collect()
}

/// Forgets a remembered device, identified by its key fingerprint.
#[tauri::command]
pub fn forget_remembered(state: State<'_, AppState>, fingerprint: String) -> Result<(), String> {
    let security = state.security().ok_or("This device's key is unavailable")?;
    let mut trust = security.trust();
    let key = trust
        .devices()
        .iter()
        .find(|device| device.fingerprint().as_deref() == Some(fingerprint.as_str()))
        .and_then(|device| device.key())
        .ok_or("That device is not in the list")?;
    trust.forget(&key).map_err(|e| e.to_string())?;
    tracing::info!(fingerprint, "remembered device forgotten by the user");
    Ok(())
}
