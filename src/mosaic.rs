use crate::{
    error::AppError,
    source::Source,
    tile::{BUFFER, SIZE, TILE, TileWindow},
};

pub struct Mosaic {
    /// Heights of the buffered window, row-major, NaN where no source has data.
    pub heights: Vec<f32>,
    /// Keys of the sources that filled a pixel of the tile itself.
    pub credited: Vec<String>,
}

/// Fills the window from `sources` in priority order, each only where every
/// higher-priority source has no data.
pub fn read(sources: &[Source], w: &TileWindow) -> Result<Option<Mosaic>, AppError> {
    let mut heights = vec![f32::NAN; SIZE * SIZE];
    let mut credited = Vec::new();

    for source in sources.iter().filter(|s| s.intersects(w)) {
        let data = source.read(w)?;
        let inner = BUFFER..BUFFER + TILE;
        let mut used = false;
        let mut missing = 0;

        let rows = heights.chunks_exact_mut(SIZE).zip(data.chunks_exact(SIZE));

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

    Ok((!credited.is_empty()).then_some(Mosaic { heights, credited }))
}
