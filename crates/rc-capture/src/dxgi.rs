//! DXGI Desktop Duplication.
//!
//! The compositor hands over each new desktop image as a GPU texture. It is
//! copied to a CPU-readable staging texture and from there into a plain
//! buffer. Duplication is lost whenever the display mode changes or the
//! secure desktop appears; the capturer rebuilds itself when that happens.

use windows::core::Interface;
use windows::Win32::Foundation::{E_ACCESSDENIED, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
    DXGI_MODE_ROTATION_ROTATE90,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_FOUND,
    DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_DESC, DXGI_OUTDUPL_FRAME_INFO,
};

use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use crate::frame::{copy_rows, Frame};
use crate::rotate::upright;
use crate::{CaptureError, DisplayInfo, Rotation};

fn rotation_of(rotation: DXGI_MODE_ROTATION) -> Rotation {
    match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => Rotation::Deg90,
        DXGI_MODE_ROTATION_ROTATE180 => Rotation::Deg180,
        DXGI_MODE_ROTATION_ROTATE270 => Rotation::Deg270,
        _ => Rotation::None,
    }
}

fn platform(e: windows::core::Error) -> CaptureError {
    if e.code() == E_ACCESSDENIED {
        CaptureError::AccessDenied
    } else if e.code() == DXGI_ERROR_UNSUPPORTED {
        CaptureError::Unsupported(e.message())
    } else {
        CaptureError::Platform(format!("{} ({:#010x})", e.message(), e.code().0))
    }
}

fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

/// Makes the current thread see physical pixels until dropped.
///
/// Windows reports display geometry scaled by the monitor's DPI setting to
/// processes that have not declared DPI awareness, while captured frames are
/// always physical. Without this, a 1920x1080 monitor at 125% is listed as
/// 1536x864.
struct PhysicalPixels(DPI_AWARENESS_CONTEXT);

impl PhysicalPixels {
    fn enter() -> Self {
        // SAFETY: takes and returns plain handles; affects this thread only.
        Self(unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) })
    }
}

impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: restores the context returned by the call in `enter`.
            unsafe { SetThreadDpiAwarenessContext(self.0) };
        }
    }
}

/// Every output attached to the desktop, with the adapter that drives it.
fn enumerate() -> Result<Vec<(IDXGIAdapter1, IDXGIOutput, DisplayInfo)>, CaptureError> {
    // SAFETY: plain COM calls on interfaces owned by this function; every
    // out-value is returned by value through the `windows` crate wrappers.
    let _physical = PhysicalPixels::enter();
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(platform)?;
        let mut found = Vec::new();

        for adapter_index in 0.. {
            let adapter = match factory.EnumAdapters1(adapter_index) {
                Ok(adapter) => adapter,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(platform(e)),
            };
            let adapter_name = wide_to_string(&adapter.GetDesc1().map_err(platform)?.Description);

            for output_index in 0.. {
                let output = match adapter.EnumOutputs(output_index) {
                    Ok(output) => output,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(platform(e)),
                };
                let desc = output.GetDesc().map_err(platform)?;
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let rect = desc.DesktopCoordinates;
                let rotation = rotation_of(desc.Rotation);
                let info = DisplayInfo {
                    index: found.len(),
                    name: wide_to_string(&desc.DeviceName),
                    adapter: adapter_name.clone(),
                    x: rect.left,
                    y: rect.top,
                    width: (rect.right - rect.left).unsigned_abs(),
                    height: (rect.bottom - rect.top).unsigned_abs(),
                    rotation,
                    primary: rect.left == 0 && rect.top == 0,
                };
                found.push((adapter.clone(), output, info));
            }
        }
        Ok(found)
    }
}

/// Lists the monitors that can be captured.
pub fn list_displays() -> Result<Vec<DisplayInfo>, CaptureError> {
    Ok(enumerate()?.into_iter().map(|(_, _, info)| info).collect())
}

struct Session {
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// How the surfaces this duplication delivers are rotated: they come in
    /// the monitor's unrotated orientation (see [`crate::rotate`]).
    rotation: Rotation,
    /// CPU-readable copy target, created on the first frame and whenever the
    /// frame size changes.
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    device: ID3D11Device,
}

impl Session {
    fn open(display: usize) -> Result<(Self, DisplayInfo), CaptureError> {
        let (adapter, output, info) = enumerate()?
            .into_iter()
            .nth(display)
            .ok_or(CaptureError::NoSuchDisplay(display))?;

        // SAFETY: the out-pointers reference live locals; the returned
        // interfaces are owned by `Session` and released on drop.
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(platform)?;
            let device: ID3D11Device =
                device.ok_or_else(|| CaptureError::Platform("no Direct3D device".into()))?;
            let context = context
                .ok_or_else(|| CaptureError::Platform("no Direct3D device context".into()))?;

            let output1: IDXGIOutput1 = output.cast().map_err(platform)?;
            let duplication = output1.DuplicateOutput(&device).map_err(platform)?;
            let duplicated: DXGI_OUTDUPL_DESC = duplication.GetDesc();

            Ok((
                Self {
                    context,
                    rotation: rotation_of(duplicated.Rotation),
                    duplication,
                    staging: None,
                    device,
                },
                info,
            ))
        }
    }

    fn staging_for(&mut self, source: &ID3D11Texture2D) -> Result<ID3D11Texture2D, CaptureError> {
        // SAFETY: `desc` is a live local filled by GetDesc; CreateTexture2D
        // writes the new texture into a live Option.
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            source.GetDesc(&mut desc);
            if let Some((texture, width, height)) = &self.staging {
                if (*width, *height) == (desc.Width, desc.Height) {
                    return Ok(texture.clone());
                }
            }

            desc.Usage = D3D11_USAGE_STAGING;
            desc.BindFlags = 0;
            desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
            desc.MiscFlags = 0;
            let mut texture = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .map_err(platform)?;
            let texture =
                texture.ok_or_else(|| CaptureError::Platform("no staging texture".into()))?;
            self.staging = Some((texture.clone(), desc.Width, desc.Height));
            Ok(texture)
        }
    }
}

/// Captures one display.
///
/// Not `Send`: create and use it on the same thread.
pub struct Capturer {
    display: usize,
    info: DisplayInfo,
    session: Option<Session>,
    buffer: Vec<u8>,
    /// The picture turned the right way up, for a rotated monitor.
    upright: Vec<u8>,
}

impl Capturer {
    /// Starts duplicating display `display` (an index from [`list_displays`]).
    pub fn new(display: usize) -> Result<Self, CaptureError> {
        let (session, info) = Session::open(display)?;
        tracing::info!(index = info.index, name = %info.name, width = info.width, height = info.height, "capture started");
        Ok(Self {
            display,
            info,
            session: Some(session),
            buffer: Vec::new(),
            upright: Vec::new(),
        })
    }

    pub fn display(&self) -> &DisplayInfo {
        &self.info
    }

    /// Waits up to `timeout_ms` for the screen to change.
    ///
    /// Returns `Ok(None)` when nothing changed in time, when only the mouse
    /// pointer moved, or when duplication had to be re-established; the
    /// caller simply calls again.
    pub fn next_frame(&mut self, timeout_ms: u32) -> Result<Option<Frame<'_>>, CaptureError> {
        if self.session.is_none() {
            // Reopening fails while the secure desktop is up; report it and
            // let the caller retry.
            let (session, info) = Session::open(self.display)?;
            tracing::info!(display = self.display, "capture re-established");
            self.info = info;
            self.session = Some(session);
        }
        let Some(session) = self.session.as_mut() else {
            return Ok(None);
        };

        // SAFETY: all pointers passed to DXGI/D3D reference live locals. The
        // mapped memory is only read between Map and Unmap, within the
        // bounds D3D reports (RowPitch * height).
        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            match session
                .duplication
                .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
            {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                    tracing::warn!(display = self.display, "capture access lost; reopening");
                    self.session = None;
                    return Ok(None);
                }
                Err(e) => return Err(platform(e)),
            }

            // A frame with no present time carries pointer movement only.
            let texture = match resource {
                Some(resource) if frame_info.LastPresentTime != 0 => {
                    resource.cast::<ID3D11Texture2D>().map_err(platform)
                }
                _ => {
                    let _ = session.duplication.ReleaseFrame();
                    return Ok(None);
                }
            };
            let copied = texture.and_then(|texture| {
                let staging = session.staging_for(&texture)?;
                session.context.CopyResource(&staging, &texture);
                Ok(staging)
            });
            let _ = session.duplication.ReleaseFrame();
            let staging = copied?;

            let (width, height) = match &session.staging {
                Some((_, width, height)) => (*width, *height),
                None => return Ok(None),
            };
            if width == 0 || height == 0 {
                return Ok(None);
            }
            let row_bytes = width as usize * Frame::BYTES_PER_PIXEL;
            self.buffer.resize(row_bytes * height as usize, 0);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            session
                .context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(platform)?;
            let pitch = mapped.RowPitch as usize;
            let source = std::slice::from_raw_parts(
                mapped.pData.cast::<u8>(),
                pitch * (height as usize - 1) + row_bytes,
            );
            copy_rows(source, pitch, &mut self.buffer, row_bytes, height as usize);
            session.context.Unmap(&staging, 0);

            // A rotated monitor delivers its picture inside an unrotated
            // surface: turn it the right way up, so the picture, its size and
            // the display's own rectangle all agree.
            let rotation = session.rotation;
            if rotation == Rotation::None {
                return Ok(Some(Frame {
                    width,
                    height,
                    data: &self.buffer,
                    accumulated_updates: frame_info.AccumulatedFrames,
                }));
            }
            let (out_width, out_height) = upright(
                &self.buffer,
                width as usize,
                height as usize,
                rotation,
                &mut self.upright,
            );
            Ok(Some(Frame {
                width: out_width as u32,
                height: out_height as u32,
                data: &self.upright,
                accumulated_updates: frame_info.AccumulatedFrames,
            }))
        }
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        tracing::info!(display = self.display, "capture stopped");
    }
}
