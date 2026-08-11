use screencapturekit::prelude::{CMSampleBuffer, CMSampleBufferExt};

use crate::frame::{
    convert_bgra_to_rgb, get_cropped_data, remove_alpha_channel, BGRAFrame, BGRFrame, RGBFrame,
    YUVFrame,
};

// Presentation timestamp in nanoseconds since CoreMedia's monotonic epoch.
pub fn get_pts_in_nanoseconds(sample: &CMSampleBuffer) -> u64 {
    sample
        .output_presentation_timestamp()
        .as_seconds()
        .map_or(0, |seconds| (seconds * 1_000_000_000.0) as u64)
}

pub fn create_yuv_frame(sample: CMSampleBuffer) -> Option<YUVFrame> {
    let display_time = get_pts_in_nanoseconds(&sample);
    let pixel_buffer = sample.image_buffer()?;
    let guard = pixel_buffer.lock_read_only().ok()?;
    let (width, height) = (guard.width(), guard.height());
    if width == 0 || height == 0 || guard.plane_count() < 2 {
        return None;
    }

    let luminance_stride = guard.bytes_per_row_of_plane(0);
    let chrominance_stride = guard.bytes_per_row_of_plane(1);
    Some(YUVFrame {
        display_time,
        width: width as i32,
        height: height as i32,
        luminance_bytes: guard.plane_data(0)?.to_vec(),
        luminance_stride: luminance_stride as i32,
        chrominance_bytes: guard.plane_data(1)?.to_vec(),
        chrominance_stride: chrominance_stride as i32,
    })
}

fn packed_frame(sample: &CMSampleBuffer) -> Option<(u64, usize, usize, Vec<u8>)> {
    let display_time = get_pts_in_nanoseconds(sample);
    let pixel_buffer = sample.image_buffer()?;
    let guard = pixel_buffer.lock_read_only().ok()?;
    let (width, height) = (guard.width(), guard.height());
    if width == 0 || height == 0 {
        return None;
    }
    let data = get_cropped_data(
        guard.as_slice().to_vec(),
        (guard.bytes_per_row() / 4) as i32,
        height as i32,
        width as i32,
    );
    Some((display_time, width, height, data))
}

pub fn create_bgr_frame(sample: CMSampleBuffer) -> Option<BGRFrame> {
    let (display_time, width, height, data) = packed_frame(&sample)?;
    Some(BGRFrame {
        display_time,
        width: width as i32,
        height: height as i32,
        data: remove_alpha_channel(data),
    })
}

pub fn create_bgra_frame(sample: CMSampleBuffer) -> Option<BGRAFrame> {
    let (display_time, width, height, data) = packed_frame(&sample)?;
    Some(BGRAFrame {
        display_time,
        width: width as i32,
        height: height as i32,
        data,
    })
}

pub fn create_rgb_frame(sample: CMSampleBuffer) -> Option<RGBFrame> {
    let (display_time, width, height, data) = packed_frame(&sample)?;
    Some(RGBFrame {
        display_time,
        width: width as i32,
        height: height as i32,
        data: convert_bgra_to_rgb(data),
    })
}
