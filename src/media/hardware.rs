use ffmpeg::ffi;
use ffmpeg_next as ffmpeg;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Nvidia,
    Vaapi,
    VideoToolbox,
}

impl Backend {
    pub const PLATFORM: &'static [Self] = if cfg!(target_os = "macos") {
        &[Self::VideoToolbox]
    } else if cfg!(target_os = "linux") {
        &[Self::Nvidia, Self::Vaapi]
    } else {
        &[Self::Nvidia]
    };

    pub fn name(self) -> &'static str {
        match self {
            Self::Nvidia => "NVENC",
            Self::Vaapi => "VAAPI",
            Self::VideoToolbox => "VideoToolbox",
        }
    }

    fn device_type(self) -> ffi::AVHWDeviceType {
        match self {
            Self::Nvidia => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA,
            Self::Vaapi => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
            Self::VideoToolbox => ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
        }
    }

    pub fn pixel(self) -> ffmpeg::format::Pixel {
        self.raw_pixel().into()
    }

    fn raw_pixel(self) -> ffi::AVPixelFormat {
        match self {
            Self::Nvidia => ffi::AVPixelFormat::AV_PIX_FMT_CUDA,
            Self::Vaapi => ffi::AVPixelFormat::AV_PIX_FMT_VAAPI,
            Self::VideoToolbox => ffi::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX,
        }
    }

    fn from_device_type(device_type: ffi::AVHWDeviceType) -> Option<Self> {
        [Self::Nvidia, Self::Vaapi, Self::VideoToolbox]
            .into_iter()
            .find(|backend| backend.device_type() == device_type)
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// A reference-counted FFmpeg hardware device. Device contexts are
/// immutable once created and FFmpeg shares them between codec contexts
/// on different threads.
pub struct Device(*mut ffi::AVBufferRef);

unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    pub fn get(backend: Backend) -> Option<&'static Self> {
        static DEVICES: [OnceLock<Option<Device>>; 3] =
            [OnceLock::new(), OnceLock::new(), OnceLock::new()];
        DEVICES[backend.index()]
            .get_or_init(|| Self::create(backend))
            .as_ref()
    }

    fn create(backend: Backend) -> Option<Self> {
        let mut device = std::ptr::null_mut();
        let result = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut device,
                backend.device_type(),
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
            )
        };
        (result >= 0 && !device.is_null()).then_some(Self(device))
    }

    pub fn reference(&self) -> Result<*mut ffi::AVBufferRef, String> {
        let reference = unsafe { ffi::av_buffer_ref(self.0) };
        if reference.is_null() {
            Err("Cannot reference hardware device".into())
        } else {
            Ok(reference)
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}

/// Opens a video decoder that decodes on the first available platform
/// device. FFmpeg falls back to software decoding through `select_format`
/// when the stream is not supported by the device.
pub fn video_decoder(
    parameters: ffmpeg::codec::Parameters,
) -> Result<ffmpeg::decoder::Video, String> {
    let mut context = ffmpeg::codec::context::Context::from_parameters(parameters)
        .map_err(|error| error.to_string())?;
    if let Some(device) = enabled().then(|| decode_device(context.id())).flatten() {
        let reference = device.reference()?;
        unsafe {
            let raw = context.as_mut_ptr();
            (*raw).hw_device_ctx = reference;
            (*raw).get_format = Some(select_format);
            // Preview readers hold the current and next frames.
            (*raw).extra_hw_frames = 2;
        }
    }
    context.decoder().video().map_err(|error| error.to_string())
}

fn decode_device(id: ffmpeg::codec::Id) -> Option<&'static Device> {
    let codec = ffmpeg::decoder::find(id)?;
    Backend::PLATFORM
        .iter()
        .copied()
        .filter(|backend| decodes_with(&codec, *backend))
        .find_map(Device::get)
}

fn decodes_with(codec: &ffmpeg::Codec, backend: Backend) -> bool {
    (0..)
        .map_while(|index| unsafe { ffi::avcodec_get_hw_config(codec.as_ptr(), index).as_ref() })
        .any(|config| {
            config.device_type == backend.device_type()
                && config.methods & ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0
        })
}

unsafe extern "C" fn select_format(
    context: *mut ffi::AVCodecContext,
    offered: *const ffi::AVPixelFormat,
) -> ffi::AVPixelFormat {
    let mut formats = Vec::new();
    let mut cursor = offered;
    unsafe {
        while *cursor != ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            formats.push(*cursor);
            cursor = cursor.add(1);
        }
    }
    let device = unsafe { (*context).hw_device_ctx };
    let hardware = (!device.is_null())
        .then(|| unsafe { (*((*device).data as *const ffi::AVHWDeviceContext)).type_ })
        .and_then(Backend::from_device_type)
        .map(Backend::raw_pixel);
    choose_format(&formats, hardware)
}

fn choose_format(
    offered: &[ffi::AVPixelFormat],
    hardware: Option<ffi::AVPixelFormat>,
) -> ffi::AVPixelFormat {
    hardware
        .filter(|format| offered.contains(format))
        .or_else(|| offered.iter().copied().find(|format| !is_hardware(*format)))
        .unwrap_or(ffi::AVPixelFormat::AV_PIX_FMT_NONE)
}

fn is_hardware(format: ffi::AVPixelFormat) -> bool {
    unsafe { ffi::av_pix_fmt_desc_get(format).as_ref() }
        .is_some_and(|descriptor| descriptor.flags & ffi::AV_PIX_FMT_FLAG_HWACCEL as u64 != 0)
}

/// Copies a frame held in device memory into system memory; software
/// frames are returned as `None` so callers can use them directly.
pub fn download(frame: &ffmpeg::frame::Video) -> Result<Option<ffmpeg::frame::Video>, String> {
    if unsafe { (*frame.as_ptr()).hw_frames_ctx.is_null() } {
        return Ok(None);
    }
    let mut software = ffmpeg::frame::Video::empty();
    unsafe {
        if ffi::av_hwframe_transfer_data(software.as_mut_ptr(), frame.as_ptr(), 0) < 0 {
            return Err("Cannot download hardware video frame".into());
        }
        if ffi::av_frame_copy_props(software.as_mut_ptr(), frame.as_ptr()) < 0 {
            return Err("Cannot copy hardware video frame properties".into());
        }
    }
    Ok(Some(software))
}

/// A pool of device frames that software frames are uploaded into before
/// encoding.
pub struct Frames(*mut ffi::AVBufferRef);

unsafe impl Send for Frames {}

impl Frames {
    pub fn new(
        device: &Device,
        backend: Backend,
        software: ffmpeg::format::Pixel,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let pool = unsafe { ffi::av_hwframe_ctx_alloc(device.0) };
        if pool.is_null() {
            return Err("Cannot allocate hardware frames".into());
        }
        let frames = Self(pool);
        unsafe {
            let context = (*pool).data as *mut ffi::AVHWFramesContext;
            (*context).format = backend.raw_pixel();
            (*context).sw_format = software.into();
            (*context).width = i32::try_from(width).map_err(|_| "Export width is out of range")?;
            (*context).height =
                i32::try_from(height).map_err(|_| "Export height is out of range")?;
            (*context).initial_pool_size = 20;
            if ffi::av_hwframe_ctx_init(pool) < 0 {
                return Err("Cannot initialize hardware frames".into());
            }
        }
        Ok(frames)
    }

    pub fn reference(&self) -> Result<*mut ffi::AVBufferRef, String> {
        let reference = unsafe { ffi::av_buffer_ref(self.0) };
        if reference.is_null() {
            Err("Cannot reference hardware frames".into())
        } else {
            Ok(reference)
        }
    }

    pub fn upload(&self, frame: &ffmpeg::frame::Video) -> Result<ffmpeg::frame::Video, String> {
        let mut uploaded = ffmpeg::frame::Video::empty();
        unsafe {
            if ffi::av_hwframe_get_buffer(self.0, uploaded.as_mut_ptr(), 0) < 0 {
                return Err("Cannot allocate hardware video frame".into());
            }
            if ffi::av_hwframe_transfer_data(uploaded.as_mut_ptr(), frame.as_ptr(), 0) < 0 {
                return Err("Cannot upload video frame to hardware".into());
            }
            if ffi::av_frame_copy_props(uploaded.as_mut_ptr(), frame.as_ptr()) < 0 {
                return Err("Cannot copy video frame properties".into());
            }
        }
        Ok(uploaded)
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        unsafe { ffi::av_buffer_unref(&mut self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffi::AVPixelFormat::{
        AV_PIX_FMT_CUDA, AV_PIX_FMT_NONE, AV_PIX_FMT_NV12, AV_PIX_FMT_YUV420P,
    };

    #[test]
    fn decoder_prefers_offered_device_format() {
        assert_eq!(
            choose_format(
                &[AV_PIX_FMT_CUDA, AV_PIX_FMT_YUV420P],
                Some(AV_PIX_FMT_CUDA)
            ),
            AV_PIX_FMT_CUDA
        );
    }

    #[test]
    fn decoder_falls_back_to_first_software_format() {
        let vaapi = Backend::Vaapi.raw_pixel();
        assert_eq!(
            choose_format(
                &[AV_PIX_FMT_CUDA, AV_PIX_FMT_NV12, AV_PIX_FMT_YUV420P],
                Some(vaapi)
            ),
            AV_PIX_FMT_NV12
        );
        assert_eq!(
            choose_format(&[AV_PIX_FMT_CUDA, AV_PIX_FMT_YUV420P], None),
            AV_PIX_FMT_YUV420P
        );
        assert_eq!(choose_format(&[AV_PIX_FMT_CUDA], None), AV_PIX_FMT_NONE);
    }

    #[test]
    fn backends_map_to_their_device_types() {
        for backend in [Backend::Nvidia, Backend::Vaapi, Backend::VideoToolbox] {
            assert_eq!(
                Backend::from_device_type(backend.device_type()),
                Some(backend)
            );
            assert!(is_hardware(backend.raw_pixel()));
        }
        assert!(!is_hardware(AV_PIX_FMT_YUV420P));
    }

    #[test]
    fn software_frames_are_not_downloaded() {
        let frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, 4, 4);
        assert!(download(&frame).unwrap().is_none());
    }
}
