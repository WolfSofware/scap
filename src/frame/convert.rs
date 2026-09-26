//! Turns a packed 32-bit capture buffer into the [Frame] a
//! [Capturer](crate::capturer::Capturer) was asked for, reading the source
//! rows in place so each frame is copied exactly once.

use super::{BGRAFrame, BGRFrame, Frame, FrameType, PixelFormat, RGBFrame, YUVFrame};

/// A borrowed image with 4 bytes per pixel and `stride` bytes per row
#[derive(Debug, Clone, Copy)]
pub(crate) struct PackedImage<'a> {
    pub data: &'a [u8],
    pub stride: usize,
    pub width: usize,
    pub height: usize,
    pub format: PixelFormat,
}

/// A pixel rectangle; `x + width` and `y + height` are exclusive ends
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Byte offsets of blue, green, red and (if present) alpha in a pixel
struct Channels {
    b: usize,
    g: usize,
    r: usize,
    a: Option<usize>,
}

fn channels(format: PixelFormat) -> Option<Channels> {
    Some(match format {
        PixelFormat::Bgra => Channels { b: 0, g: 1, r: 2, a: Some(3) },
        PixelFormat::Bgrx => Channels { b: 0, g: 1, r: 2, a: None },
        PixelFormat::Rgba => Channels { r: 0, g: 1, b: 2, a: Some(3) },
        PixelFormat::Rgbx => Channels { r: 0, g: 1, b: 2, a: None },
        PixelFormat::Xbgr => Channels { b: 1, g: 2, r: 3, a: None },
        PixelFormat::Nv12 | PixelFormat::Other(_) => return None,
    })
}

impl<'a> PackedImage<'a> {
    /// Whether `data` holds `height` rows of `width` pixels
    fn is_valid(&self) -> bool {
        let row = self.width * 4;
        self.width > 0
            && self.height > 0
            && self.stride >= row
            && self.data.len() >= self.stride * (self.height - 1) + row
    }

    fn row(&self, y: usize) -> &'a [u8] {
        let start = y * self.stride;
        &self.data[start..start + self.width * 4]
    }

    /// The part of the image inside `rect`, clamped to the image bounds
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn cropped(self, rect: Rect) -> Option<Self> {
        if !self.is_valid() {
            return None;
        }
        let x = rect.x.min(self.width);
        let y = rect.y.min(self.height);
        let width = rect.width.min(self.width - x);
        let height = rect.height.min(self.height - y);
        if width == 0 || height == 0 {
            return None;
        }
        Some(Self {
            data: &self.data[y * self.stride + x * 4..],
            width,
            height,
            ..self
        })
    }
}

/// Converts `image` into the frame type the caller asked for; `None` if the
/// image is empty, malformed or in a format without a byte layout
pub(crate) fn to_frame(image: PackedImage<'_>, output: FrameType, display_time: u64) -> Option<Frame> {
    if !image.is_valid() {
        return None;
    }
    let channels = channels(image.format)?;
    let (width, height) = (image.width as i32, image.height as i32);

    Some(match output {
        FrameType::BGRAFrame => Frame::BGRA(BGRAFrame {
            display_time,
            width,
            height,
            data: to_bgra(&image, &channels),
        }),
        FrameType::BGR0 => Frame::BGR0(BGRFrame {
            display_time,
            width,
            height,
            data: to_packed3(&image, [channels.b, channels.g, channels.r]),
        }),
        FrameType::RGB => Frame::RGB(RGBFrame {
            display_time,
            width,
            height,
            data: to_packed3(&image, [channels.r, channels.g, channels.b]),
        }),
        FrameType::YUVFrame => Frame::YUVFrame(to_nv12(&image, &channels, display_time)?),
    })
}

fn to_bgra(image: &PackedImage<'_>, channels: &Channels) -> Vec<u8> {
    let mut data = Vec::with_capacity(image.width * image.height * 4);
    // Already B, G, R, A: copy whole rows
    if image.format == PixelFormat::Bgra {
        for y in 0..image.height {
            data.extend_from_slice(image.row(y));
        }
        return data;
    }
    for y in 0..image.height {
        for pixel in image.row(y).chunks_exact(4) {
            data.extend_from_slice(&[
                pixel[channels.b],
                pixel[channels.g],
                pixel[channels.r],
                channels.a.map_or(u8::MAX, |a| pixel[a]),
            ]);
        }
    }
    data
}

fn to_packed3(image: &PackedImage<'_>, order: [usize; 3]) -> Vec<u8> {
    let mut data = Vec::with_capacity(image.width * image.height * 3);
    for y in 0..image.height {
        for pixel in image.row(y).chunks_exact(4) {
            data.extend_from_slice(&[pixel[order[0]], pixel[order[1]], pixel[order[2]]]);
        }
    }
    data
}

/// BT.709 limited range NV12, the same layout ScreenCaptureKit delivers.
/// Odd widths and heights lose their last column or row.
fn to_nv12(image: &PackedImage<'_>, channels: &Channels, display_time: u64) -> Option<YUVFrame> {
    let width = image.width & !1;
    let height = image.height & !1;
    if width == 0 || height == 0 {
        return None;
    }

    let rgb = |pixel: &[u8]| {
        (
            i32::from(pixel[channels.r]),
            i32::from(pixel[channels.g]),
            i32::from(pixel[channels.b]),
        )
    };
    let luma = |(r, g, b): (i32, i32, i32)| (((47 * r + 157 * g + 16 * b + 128) >> 8) + 16) as u8;

    let mut luminance = Vec::with_capacity(width * height);
    let mut chrominance = Vec::with_capacity(width * height / 2);
    for y in (0..height).step_by(2) {
        let (top, bottom) = (image.row(y), image.row(y + 1));
        for row in [top, bottom] {
            luminance.extend(row[..width * 4].chunks_exact(4).map(|pixel| luma(rgb(pixel))));
        }
        for x in (0..width).step_by(2) {
            // Average the 2x2 block for its shared chroma sample
            let (mut r, mut g, mut b) = (0, 0, 0);
            for row in [top, bottom] {
                for pixel in row[x * 4..x * 4 + 8].chunks_exact(4) {
                    let (pr, pg, pb) = rgb(pixel);
                    r += pr;
                    g += pg;
                    b += pb;
                }
            }
            let (r, g, b) = (r / 4, g / 4, b / 4);
            chrominance.push((((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128) as u8);
            chrominance.push((((112 * r - 102 * g - 10 * b + 128) >> 8) + 128) as u8);
        }
    }

    Some(YUVFrame {
        display_time,
        width: width as i32,
        height: height as i32,
        luminance_bytes: luminance,
        luminance_stride: width as i32,
        chrominance_bytes: chrominance,
        chrominance_stride: width as i32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3x2 image with 4 bytes of row padding
    fn image() -> Vec<u8> {
        let mut data = Vec::new();
        for row in 0..2u8 {
            for column in 0..3u8 {
                let base = row * 30 + column * 10;
                data.extend_from_slice(&[base + 1, base + 2, base + 3, base + 4]);
            }
            data.extend_from_slice(&[0xee; 4]);
        }
        data
    }

    fn packed(data: &[u8], format: PixelFormat) -> PackedImage<'_> {
        PackedImage {
            data,
            stride: 16,
            width: 3,
            height: 2,
            format,
        }
    }

    fn bytes(frame: Frame) -> Vec<u8> {
        match frame {
            Frame::BGRA(frame) => frame.data,
            Frame::BGR0(frame) => frame.data,
            Frame::RGB(frame) => frame.data,
            frame => panic!("unexpected {frame:?}"),
        }
    }

    #[test]
    fn bgra_rows_are_copied_without_padding() {
        let data = image();
        let frame = to_frame(packed(&data, PixelFormat::Bgra), FrameType::BGRAFrame, 7).unwrap();
        assert_eq!(
            bytes(frame),
            vec![1, 2, 3, 4, 11, 12, 13, 14, 21, 22, 23, 24, 31, 32, 33, 34, 41, 42, 43, 44, 51, 52, 53, 54]
        );
    }

    #[test]
    fn padding_formats_get_opaque_alpha_and_reordered_channels() {
        let data = image();
        let frame = to_frame(packed(&data, PixelFormat::Rgbx), FrameType::BGRAFrame, 0).unwrap();
        assert_eq!(&bytes(frame)[..8], &[3, 2, 1, 255, 13, 12, 11, 255]);

        let data = image();
        let frame = to_frame(packed(&data, PixelFormat::Xbgr), FrameType::RGB, 0).unwrap();
        assert_eq!(&bytes(frame)[..6], &[4, 3, 2, 14, 13, 12]);
    }

    #[test]
    fn three_byte_outputs_drop_alpha() {
        let data = image();
        let bgr = bytes(to_frame(packed(&data, PixelFormat::Bgra), FrameType::BGR0, 0).unwrap());
        let rgb = bytes(to_frame(packed(&data, PixelFormat::Bgra), FrameType::RGB, 0).unwrap());
        assert_eq!(&bgr[..6], &[1, 2, 3, 11, 12, 13]);
        assert_eq!(&rgb[..6], &[3, 2, 1, 13, 12, 11]);
        assert_eq!(bgr.len(), 3 * 2 * 3);
    }

    #[test]
    fn crop_is_clamped_to_the_image() {
        let data = image();
        let cropped = packed(&data, PixelFormat::Bgra)
            .cropped(Rect {
                x: 1,
                y: 1,
                width: 10,
                height: 10,
            })
            .unwrap();
        assert_eq!((cropped.width, cropped.height), (2, 1));
        let frame = to_frame(cropped, FrameType::BGRAFrame, 0).unwrap();
        assert_eq!(bytes(frame), vec![41, 42, 43, 44, 51, 52, 53, 54]);

        let outside = Rect {
            x: 5,
            y: 0,
            width: 2,
            height: 2,
        };
        assert!(packed(&data, PixelFormat::Bgra).cropped(outside).is_none());
    }

    #[test]
    fn nv12_uses_bt709_limited_range() {
        let white = [255u8; 16];
        let black = [0u8; 16];
        for (data, luma) in [(white, 235), (black, 16)] {
            let image = PackedImage {
                data: &data,
                stride: 8,
                width: 2,
                height: 2,
                format: PixelFormat::Bgrx,
            };
            let Some(Frame::YUVFrame(frame)) = to_frame(image, FrameType::YUVFrame, 0) else {
                panic!("expected NV12");
            };
            assert_eq!(frame.luminance_bytes, vec![luma; 4]);
            assert_eq!(frame.chrominance_bytes, vec![128, 128]);
        }
    }

    #[test]
    fn nv12_drops_odd_edges_and_short_buffers_are_rejected() {
        let data = image();
        let Some(Frame::YUVFrame(frame)) =
            to_frame(packed(&data, PixelFormat::Bgra), FrameType::YUVFrame, 0)
        else {
            panic!("expected NV12");
        };
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(frame.luminance_bytes.len(), 4);
        assert_eq!(frame.chrominance_bytes.len(), 2);

        let short = &data[..20];
        assert!(to_frame(packed(short, PixelFormat::Bgra), FrameType::BGRAFrame, 0).is_none());
        assert!(to_frame(packed(&data, PixelFormat::Nv12), FrameType::BGRAFrame, 0).is_none());
    }
}
