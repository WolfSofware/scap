//! Zero-copy frames handed to subscription callbacks.
//!
//! A [VideoFrame] borrows the capture API's own GPU (or shared memory)
//! buffer and is only valid for the duration of the callback. Keep what you
//! need by retaining the native handle (macOS), copying the texture on the
//! GPU (Windows) or importing the DMA-BUF (Linux) before returning.

/// Pixel layout of a [VideoFrame]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 8-bit B, G, R, A
    Bgra,
    /// 8-bit B, G, R, padding
    Bgrx,
    /// 8-bit R, G, B, A
    Rgba,
    /// 8-bit R, G, B, padding
    Rgbx,
    /// 8-bit padding, B, G, R
    Xbgr,
    /// Biplanar 4:2:0 Y'CbCr (NV12)
    Nv12,
    /// A format scap does not name, as the platform's raw identifier
    /// (DRM fourcc on Linux, `DXGI_FORMAT` on Windows, `OSType` on macOS)
    Other(u32),
}

/// A captured video frame that still lives in the capture API's buffer
#[derive(Debug)]
pub struct VideoFrame<'a> {
    /// Presentation time in nanoseconds on the platform's monotonic clock
    /// (CoreMedia host time, QPC or `CLOCK_MONOTONIC`); comparable with
    /// [AudioFrame::display_time] of the same subscription
    pub display_time: u64,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub native: NativeFrame<'a>,
}

/// Platform handle of a [VideoFrame]
#[derive(Debug)]
pub enum NativeFrame<'a> {
    /// IOSurface-backed `CVPixelBuffer` wrapped in its `CMSampleBuffer`.
    /// Cloning the sample buffer retains it past the callback.
    #[cfg(target_os = "macos")]
    CoreVideo(&'a screencapturekit::cm::CMSampleBuffer),

    /// Texture from the Windows.Graphics.Capture frame pool. The pool reuses
    /// it after the callback returns, so `CopyResource` it to keep the image.
    #[cfg(target_os = "windows")]
    Direct3D11 {
        texture: &'a windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        device: &'a windows::Win32::Graphics::Direct3D11::ID3D11Device,
        context: &'a windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    },

    /// GPU buffer shared by the compositor through PipeWire
    #[cfg(target_os = "linux")]
    DmaBuf(DmaBuf<'a>),

    /// CPU memory shared with the producer (PipeWire memfd or X11 MIT-SHM
    /// segment); mapped, not copied
    #[cfg(target_os = "linux")]
    Memory { data: &'a [u8], stride: u32 },
}

/// A DMA-BUF frame, ready for `EGL_EXT_image_dma_buf_import`,
/// `VK_EXT_external_memory_dma_buf` or FFmpeg's `AV_PIX_FMT_DRM_PRIME`
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct DmaBuf<'a> {
    /// DRM fourcc (`DRM_FORMAT_*`)
    pub fourcc: u32,
    /// DRM format modifier (`DRM_FORMAT_MOD_*`)
    pub modifier: u64,
    pub(crate) planes: [DmaBufPlane<'a>; 4],
    pub(crate) plane_count: usize,
}

#[cfg(target_os = "linux")]
impl<'a> DmaBuf<'a> {
    pub fn planes(&self) -> &[DmaBufPlane<'a>] {
        &self.planes[..self.plane_count]
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy)]
pub struct DmaBufPlane<'a> {
    pub fd: std::os::fd::BorrowedFd<'a>,
    pub offset: u32,
    pub stride: u32,
}

/// Sample encoding of an [AudioFrame]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSampleFormat {
    F32,
    I16,
    I32,
}

impl AudioSampleFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            AudioSampleFormat::I16 => 2,
            AudioSampleFormat::F32 | AudioSampleFormat::I32 => 4,
        }
    }
}

/// Captured audio that still lives in the capture API's buffer
#[derive(Debug)]
pub struct AudioFrame<'a> {
    /// Presentation time in nanoseconds, same clock as [VideoFrame::display_time]
    pub display_time: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: AudioSampleFormat,
    /// Number of samples per channel
    pub frames: usize,
    pub data: AudioData<'a>,
}

/// Sample memory of an [AudioFrame]
#[derive(Debug)]
pub enum AudioData<'a> {
    /// All channels interleaved in one buffer
    Interleaved(&'a [u8]),
    /// One buffer per channel
    Planar(&'a [&'a [u8]]),
}
