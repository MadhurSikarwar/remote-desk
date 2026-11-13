//! Commands the UI may invoke. They only forward to the controllers, which
//! hold the logic and its tests.

use std::time::Duration;

use rc_core::Config;
use rc_crypto::{fingerprint, from_hex, to_hex};
use serde::Serialize;
use std::sync::Arc;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

use crate::rendezvous::ServerLink;
use crate::share::{DisplaySummary, ShareController, ShareSettings, ShareStatus};
use crate::state::{AppState, Status};
use crate::transfer::{Direction, TransferStatus, Transfers};
use crate::viewer::{ConnectOptions, HostInfo, ViewerController, ViewerInput, ViewerStatus};

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
        allow_clipboard: config.security.allow_clipboard,
        allow_files: config.security.allow_file_transfer,
        received_folder: state.received_folder(),
        clipboard: None,
        server: ServerLink::from_config(&config.network)?,
        advertised_port: None,
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
    let config = state.config();
    let options = ConnectOptions {
        expected_key: expected,
        server: ServerLink::from_config(&config.network)?,
        policy: config.network.transport,
        received_folder: state.received_folder(),
        clipboard: None,
    };
    viewer.connect_with(&address, &options, &config.device.name, move |packet| {
        // Fails only once the window is gone; the session ends with it.
        let _ = packets.send(InvokeResponseBody::Raw(packet));
    })
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

/// The host's switch for clipboard text sync.
#[tauri::command]
pub fn set_share_clipboard(share: State<'_, ShareController>, allowed: bool) -> ShareStatus {
    let status = share.set_clipboard_allowed(allowed);
    tracing::info!(allowed, "clipboard sync switched by the user");
    status
}

/// The host's switch for file transfer.
#[tauri::command]
pub fn set_share_files(share: State<'_, ShareController>, allowed: bool) -> ShareStatus {
    let status = share.set_files_allowed(allowed);
    tracing::info!(allowed, "file transfer switched by the user");
    status
}

/// The viewer's own switch for clipboard text sync, which also needs the
/// host to allow it.
#[tauri::command]
pub fn set_view_clipboard(viewer: State<'_, ViewerController>, wanted: bool) -> Result<(), String> {
    viewer.set_clipboard_wanted(wanted)?;
    tracing::info!(wanted, "clipboard sync switched by the user (viewer)");
    Ok(())
}

#[tauri::command]
pub fn view_capability_status(viewer: State<'_, ViewerController>) -> Option<ViewerStatus> {
    viewer.status()
}

/// The file transfers of the host's or the viewer's session.
fn transfers_of(
    role: &str,
    share: &ShareController,
    viewer: &ViewerController,
) -> Result<Arc<Transfers>, String> {
    let capabilities = match role {
        "host" => share.capabilities(),
        "viewer" => viewer.capabilities(),
        _ => return Err("Unknown role".to_owned()),
    };
    capabilities
        .map(|c| Arc::clone(&c.transfers))
        .ok_or_else(|| "There is no session to transfer files in".to_owned())
}

#[tauri::command]
pub fn transfer_status(
    role: String,
    share: State<'_, ShareController>,
    viewer: State<'_, ViewerController>,
) -> Result<TransferStatus, String> {
    Ok(transfers_of(&role, &share, &viewer)?.status())
}

/// The person's answer to a file offered by the other end. Only ever called
/// from the Accept and Refuse buttons.
#[tauri::command]
pub fn transfer_answer(
    role: String,
    id: u32,
    accept: bool,
    share: State<'_, ShareController>,
    viewer: State<'_, ViewerController>,
) -> Result<(), String> {
    transfers_of(&role, &share, &viewer)?.answer(id, accept)?;
    tracing::info!(role, id, accept, "file offer answered by the user");
    Ok(())
}

/// Offers a file the person picked. Returns the transfer's number.
#[tauri::command]
pub fn transfer_offer(
    role: String,
    name: String,
    size: u64,
    share: State<'_, ShareController>,
    viewer: State<'_, ViewerController>,
) -> Result<u32, String> {
    transfers_of(&role, &share, &viewer)?.offer(&name, size)
}

/// Sends the next piece of a file. The bytes are the request body; `role`
/// and `id` are headers.
#[tauri::command(async)]
pub fn transfer_chunk(
    request: tauri::ipc::Request<'_>,
    share: State<'_, ShareController>,
    viewer: State<'_, ViewerController>,
) -> Result<(), String> {
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("The file data must be sent as raw bytes".to_owned());
    };
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| format!("missing {name} header"))
    };
    let role = header("x-role")?;
    let id: u32 = header("x-id")?
        .parse()
        .map_err(|_| "bad transfer number".to_owned())?;
    transfers_of(&role, &share, &viewer)?.chunk(id, bytes)
}

#[tauri::command]
pub fn transfer_finish(
    role: String,
    id: u32,
    share: State<'_, ShareController>,
    viewer: State<'_, ViewerController>,
) -> Result<(), String> {
    transfers_of(&role, &share, &viewer)?.finish(id)
}

#[tauri::command]
pub fn transfer_cancel(
    role: String,
    direction: Direction,
    id: u32,
    share: State<'_, ShareController>,
    viewer: State<'_, ViewerController>,
) -> Result<(), String> {
    transfers_of(&role, &share, &viewer)?.cancel(direction, id);
    Ok(())
}
