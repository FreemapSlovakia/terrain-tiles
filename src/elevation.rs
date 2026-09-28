use crate::{error::AppError, tile::SIZE};

/// Height step at z18 and above. It doubles per zoom below, keeping the step
/// the same fraction of the pixel.
const STEP_Z18: f64 = 0.002;

/// Encodes heights as zstd over: `f32` step in metres, `u32` side in pixels,
/// then row-major `i32` heights in steps, each row delta-coded from 0.
/// `i32::MIN` before delta coding is no data; the deltas wrap.
pub fn encode(heights: &[f32], z: u8) -> Result<Vec<u8>, AppError> {
    let step = STEP_Z18 * 2f64.powi((18 - z as i32).max(0));

    let mut raw = Vec::with_capacity(8 + heights.len() * 4);

    raw.extend_from_slice(&(step as f32).to_le_bytes());
    raw.extend_from_slice(&(SIZE as u32).to_le_bytes());

    for row in heights.chunks_exact(SIZE) {
        let mut prev = 0i32;

        for &h in row {
            let q = if h.is_nan() {
                i32::MIN
            } else {
                (h as f64 / step).round() as i32
            };

            raw.extend_from_slice(&q.wrapping_sub(prev).to_le_bytes());

            prev = q;
        }
    }

    Ok(zstd::bulk::compress(&raw, 3)?)
}
