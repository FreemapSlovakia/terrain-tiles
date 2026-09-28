use crate::{error::AppError, shading::Planes, tile::TILE};
use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};
use serde::Deserialize;

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Png,
    #[serde(alias = "jpg")]
    Jpeg,
    Webp,
}

/// Encodes `planes` as `format`, one channel fewer when `gray`. Returns the
/// body and its content type.
pub fn encode(
    planes: &Planes,
    format: Format,
    gray: bool,
) -> Result<(Vec<u8>, &'static str), AppError> {
    let mut body = Vec::new();

    let content_type = match format {
        Format::Jpeg => {
            // JPEG has no alpha: composite over white.
            let encoder = jpeg_encoder::Encoder::new(&mut body, 90);
            let side = TILE as u16;

            if gray {
                encoder.encode(
                    &planes.gray_over_white(),
                    side,
                    side,
                    jpeg_encoder::ColorType::Luma,
                )?;
            } else {
                encoder.encode(
                    &planes.rgb_over_white(),
                    side,
                    side,
                    jpeg_encoder::ColorType::Rgb,
                )?;
            }

            "image/jpeg"
        }
        Format::Png => {
            let (data, color) = if gray {
                (planes.gray_alpha(), ExtendedColorType::La8)
            } else {
                (planes.rgba(), ExtendedColorType::Rgba8)
            };

            PngEncoder::new(&mut body).write_image(&data, TILE as u32, TILE as u32, color)?;

            "image/png"
        }
        Format::Webp => {
            let encoded = webp::Encoder::from_rgba(&planes.rgba(), TILE as u32, TILE as u32)
                .encode_simple(false, 90.0)
                .map_err(|e| AppError::Encode(format!("webp: {e:?}")))?;

            body.extend_from_slice(&encoded);

            "image/webp"
        }
    };

    Ok((body, content_type))
}
