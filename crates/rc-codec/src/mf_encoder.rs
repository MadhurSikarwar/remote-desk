//! H.264 encoding through Media Foundation.
//!
//! Uses Microsoft's software H.264 encoder, a synchronous transform: feed one
//! NV12 sample, then drain whatever output is ready. Low-latency mode makes it
//! emit each frame as soon as it is given, without reordering.

use std::mem::ManuallyDrop;

use windows::core::{Interface, GUID};
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_Main, CMSH264EncoderMFT,
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVGOPSize,
    CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMFMediaType,
    IMFSample, IMFTransform, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSampleExtension_CleanPoint, MFShutdown, MFStartup, MFVideoFormat_H264,
    MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFSTARTUP_LITE,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_MT_AVG_BITRATE,
    MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_VERSION,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};

use windows::Win32::System::Variant::VARIANT;

use crate::nv12::nv12_len;
use crate::{CodecError, EncodedFrame, EncoderConfig};

/// A keyframe at least this often, so a viewer that lost data recovers.
const KEYFRAME_INTERVAL_SECS: u32 = 4;

fn platform(e: windows::core::Error) -> CodecError {
    CodecError::Platform(format!("{} ({:#010x})", e.message(), e.code().0))
}

/// Sets an encoder property.
///
/// # Safety
/// `codec_api` must be a live interface.
unsafe fn set_property(
    codec_api: &ICodecAPI,
    property: &GUID,
    value: VARIANT,
) -> Result<(), CodecError> {
    unsafe { codec_api.SetValue(property, &value).map_err(platform) }
}

fn pack(high: u32, low: u32) -> u64 {
    (u64::from(high) << 32) | u64::from(low)
}

/// Balances COM and Media Foundation start-up with shutdown on this thread.
struct Runtime {
    com_initialised: bool,
}

impl Runtime {
    fn start() -> Result<Self, CodecError> {
        // SAFETY: plain initialisation calls; each success is undone in Drop.
        unsafe {
            // S_FALSE (already initialised) and RPC_E_CHANGED_MODE (another
            // apartment model) both leave COM usable on this thread; only a
            // successful call of ours must be balanced.
            let com_initialised = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_LITE) {
                if com_initialised {
                    CoUninitialize();
                }
                return Err(platform(e));
            }
            Ok(Self { com_initialised })
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful calls in `start`.
        unsafe {
            let _ = MFShutdown();
            if self.com_initialised {
                CoUninitialize();
            }
        }
    }
}

/// Encodes NV12 frames to H.264. Create and use it on one thread.
pub struct H264Encoder {
    // Field order matters: COM objects are released before the runtime.
    codec_api: ICodecAPI,
    transform: IMFTransform,
    config: EncoderConfig,
    /// The bit rate in force now, which `set_bitrate_kbps` may have changed
    /// from the one in `config`.
    bitrate_kbps: u32,
    frame_duration_100ns: i64,
    _runtime: Runtime,
}

impl H264Encoder {
    pub fn new(config: EncoderConfig) -> Result<Self, CodecError> {
        if config.width == 0 || config.height == 0 {
            return Err(CodecError::InvalidConfig(
                "width and height must be non-zero",
            ));
        }
        if !config.width.is_multiple_of(2) || !config.height.is_multiple_of(2) {
            return Err(CodecError::InvalidConfig("width and height must be even"));
        }
        if config.fps == 0 || config.bitrate_kbps == 0 {
            return Err(CodecError::InvalidConfig(
                "fps and bitrate must be non-zero",
            ));
        }
        let bitrate = config.bitrate_kbps.saturating_mul(1000);
        let runtime = Runtime::start()?;

        // SAFETY: COM calls on interfaces created here; every pointer passed
        // references a live local.
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CMSH264EncoderMFT, None, CLSCTX_INPROC_SERVER)
                    .map_err(platform)?;
            let codec_api: ICodecAPI = transform.cast().map_err(platform)?;

            // Encoder properties must be set before the media types.
            let set = |property: &GUID, value: VARIANT| set_property(&codec_api, property, value);
            set(&CODECAPI_AVLowLatencyMode, VARIANT::from(true))?;
            set(
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
            )?;
            set(&CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bitrate))?;
            set(
                &CODECAPI_AVEncMPVGOPSize,
                VARIANT::from(config.fps.saturating_mul(KEYFRAME_INTERVAL_SECS)),
            )?;

            let video_type = |subtype: &GUID| -> Result<IMFMediaType, CodecError> {
                let media_type = MFCreateMediaType().map_err(platform)?;
                let apply = || -> windows::core::Result<()> {
                    media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                    media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(config.width, config.height))?;
                    media_type.SetUINT64(&MF_MT_FRAME_RATE, pack(config.fps, 1))?;
                    media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
                    media_type
                        .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                };
                apply().map_err(platform)?;
                Ok(media_type)
            };

            // The encoder requires its output type before its input type.
            let output = video_type(&MFVideoFormat_H264)?;
            output
                .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
                .and_then(|()| {
                    output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)
                })
                .map_err(platform)?;
            transform.SetOutputType(0, &output, 0).map_err(platform)?;
            let input = video_type(&MFVideoFormat_NV12)?;
            transform.SetInputType(0, &input, 0).map_err(platform)?;

            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .and_then(|()| transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0))
                .map_err(platform)?;

            tracing::info!(
                width = config.width,
                height = config.height,
                fps = config.fps,
                bitrate_kbps = config.bitrate_kbps,
                "H.264 encoder ready"
            );
            Ok(Self {
                codec_api,
                transform,
                config,
                bitrate_kbps: config.bitrate_kbps,
                frame_duration_100ns: 10_000_000 / i64::from(config.fps),
                _runtime: runtime,
            })
        }
    }

    pub fn config(&self) -> EncoderConfig {
        self.config
    }

    /// The bit rate being aimed for now, in kilobits per second.
    pub fn bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps
    }

    /// Changes the bit rate to aim for, while encoding. The encoder applies
    /// it from a following frame and carries on; no keyframe is needed. This
    /// does not change [`config`](Self::config), which stays what the
    /// encoder was created with.
    pub fn set_bitrate_kbps(&mut self, kbps: u32) -> Result<(), CodecError> {
        if kbps == 0 {
            return Err(CodecError::InvalidConfig("bitrate must be non-zero"));
        }
        // SAFETY: `codec_api` is owned by `self` and alive.
        unsafe {
            set_property(
                &self.codec_api,
                &CODECAPI_AVEncCommonMeanBitRate,
                VARIANT::from(kbps.saturating_mul(1000)),
            )?;
        }
        self.bitrate_kbps = kbps;
        Ok(())
    }

    /// Makes the next encoded frame a keyframe, for a viewer that just
    /// connected or reported lost data.
    pub fn request_keyframe(&mut self) -> Result<(), CodecError> {
        // SAFETY: `codec_api` is owned by `self` and alive.
        unsafe {
            set_property(
                &self.codec_api,
                &CODECAPI_AVEncVideoForceKeyFrame,
                VARIANT::from(1u32),
            )
        }
    }

    /// Encodes one NV12 frame. In low-latency mode each call normally yields
    /// exactly one encoded frame; the result is a list because the encoder is
    /// free to yield none or several.
    pub fn encode(
        &mut self,
        nv12: &[u8],
        timestamp_us: u64,
    ) -> Result<Vec<EncodedFrame>, CodecError> {
        let expected = nv12_len(self.config.width as usize, self.config.height as usize);
        if nv12.len() != expected {
            return Err(CodecError::WrongFrameSize {
                expected,
                actual: nv12.len(),
            });
        }
        let length =
            u32::try_from(expected).map_err(|_| CodecError::InvalidConfig("frame too large"))?;

        // SAFETY: the locked buffer is `length` bytes (requested above) and
        // is written only between Lock and Unlock.
        unsafe {
            let buffer = MFCreateMemoryBuffer(length).map_err(platform)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None).map_err(platform)?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), data, expected);
            buffer.Unlock().map_err(platform)?;
            buffer.SetCurrentLength(length).map_err(platform)?;

            let sample = MFCreateSample().map_err(platform)?;
            sample.AddBuffer(&buffer).map_err(platform)?;
            sample
                .SetSampleTime(i64::try_from(timestamp_us).unwrap_or(i64::MAX / 10) * 10)
                .and_then(|()| sample.SetSampleDuration(self.frame_duration_100ns))
                .map_err(platform)?;

            self.transform
                .ProcessInput(0, &sample, 0)
                .map_err(platform)?;
        }
        self.drain()
    }

    fn drain(&mut self) -> Result<Vec<EncodedFrame>, CodecError> {
        let mut frames = Vec::new();
        // SAFETY: the output sample is wrapped in ManuallyDrop by the API and
        // is taken back (and so released) on every path below. Locked buffer
        // memory is read only between Lock and Unlock, within its length.
        unsafe {
            loop {
                let info = self.transform.GetOutputStreamInfo(0).map_err(platform)?;
                let provides_samples =
                    info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
                let sample = if provides_samples {
                    None
                } else {
                    let sample = MFCreateSample().map_err(platform)?;
                    let buffer = MFCreateMemoryBuffer(info.cbSize.max(1)).map_err(platform)?;
                    sample.AddBuffer(&buffer).map_err(platform)?;
                    Some(sample)
                };

                let mut outputs = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(sample),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let result = self.transform.ProcessOutput(0, &mut outputs, &mut status);
                let [output] = outputs;
                let sample: Option<IMFSample> = ManuallyDrop::into_inner(output.pSample);
                drop(ManuallyDrop::into_inner(output.pEvents));

                match result {
                    Ok(()) => {}
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => break,
                    Err(e) => return Err(platform(e)),
                }
                let Some(sample) = sample else { continue };

                let buffer = sample.ConvertToContiguousBuffer().map_err(platform)?;
                let mut data = std::ptr::null_mut();
                let mut length = 0u32;
                buffer
                    .Lock(&mut data, None, Some(&mut length))
                    .map_err(platform)?;
                let bytes = std::slice::from_raw_parts(data, length as usize).to_vec();
                buffer.Unlock().map_err(platform)?;

                let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
                let timestamp_us =
                    u64::try_from(sample.GetSampleTime().unwrap_or(0) / 10).unwrap_or(0);
                frames.push(EncodedFrame {
                    data: bytes,
                    keyframe,
                    timestamp_us,
                });
            }
        }
        Ok(frames)
    }
}
