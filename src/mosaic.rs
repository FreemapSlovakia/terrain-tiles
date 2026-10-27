use crate::{
    error::AppError,
    source::Source,
    tile::{TILE, TileWindow},
};

pub struct Mosaic {
    /// Heights of the buffered window, row-major, NaN where no source has data.
    pub heights: Vec<f32>,
    /// Per window pixel, the native zoom of the source that filled it;
    /// `u8::MAX` where none did. Empty unless asked for.
    pub native: Vec<u8>,
    /// Keys of the sources that filled a pixel of the tile itself.
    pub credited: Vec<String>,
}

/// Fills the window from `sources` in priority order, each only where every
/// higher-priority source has no data. `with_native` fills `Mosaic::native`.
pub fn read(
    sources: &[Source],
    w: &TileWindow,
    with_native: bool,
) -> Result<Option<Mosaic>, AppError> {
    let size = w.size();
    let mut heights = vec![f32::NAN; size * size];
    let mut native = if with_native {
        vec![u8::MAX; size * size]
    } else {
        Vec::new()
    };
    let mut credited = Vec::new();

    for source in sources.iter().filter(|s| s.intersects(w)) {
        let data = source.read(w)?;
        let inner = w.buffer..w.buffer + TILE;
        let mut used = false;
        let mut missing = 0;

        let rows = heights.chunks_exact_mut(size).zip(data.chunks_exact(size));

        for (y, (row, new)) in rows.enumerate() {
            let in_tile = inner.contains(&y);

            for (x, (h, v)) in row.iter_mut().zip(new).enumerate() {
                if !h.is_nan() {
                    continue;
                }

                if v.is_nan() {
                    missing += 1;
                } else {
                    *h = *v;
                    if let Some(z) = native.get_mut(y * size + x) {
                        *z = source.zoom;
                    }

                    used |= in_tile && inner.contains(&x);
                }
            }
        }

        if used {
            credited.push(source.key.clone());
        }

        if missing == 0 {
            break;
        }
    }

    Ok((!credited.is_empty()).then_some(Mosaic {
        heights,
        native,
        credited,
    }))
}
