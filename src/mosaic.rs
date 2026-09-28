use crate::{
    error::AppError,
    source::Source,
    tile::{BUFFER, SIZE, TILE, TileWindow},
};

pub struct Mosaic {
    /// Heights of the buffered window, row-major, NaN where no source has data.
    pub heights: Vec<f32>,
    /// Per window pixel, the native zoom of the source that filled it;
    /// `u8::MAX` where none did.
    pub native: Vec<u8>,
    /// Keys of the sources that filled a pixel of the tile itself.
    pub credited: Vec<String>,
}

/// Fills the window from `sources` in priority order, each only where every
/// higher-priority source has no data.
pub fn read(sources: &[Source], w: &TileWindow) -> Result<Option<Mosaic>, AppError> {
    let mut heights = vec![f32::NAN; SIZE * SIZE];
    let mut native = vec![u8::MAX; SIZE * SIZE];
    let mut credited = Vec::new();

    for source in sources.iter().filter(|s| s.intersects(w)) {
        let data = source.read(w)?;
        let inner = BUFFER..BUFFER + TILE;
        let mut used = false;
        let mut missing = 0;

        let rows = heights
            .chunks_exact_mut(SIZE)
            .zip(native.chunks_exact_mut(SIZE))
            .zip(data.chunks_exact(SIZE));

        for (y, ((row, zooms), new)) in rows.enumerate() {
            let in_tile = inner.contains(&y);

            for (x, ((h, z), v)) in row.iter_mut().zip(zooms).zip(new).enumerate() {
                if !h.is_nan() {
                    continue;
                }

                if v.is_nan() {
                    missing += 1;
                } else {
                    *h = *v;
                    *z = source.zoom;
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
