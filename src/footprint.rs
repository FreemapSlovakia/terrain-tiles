//! Where a source actually has data, from its coarsest overview: a tile that
//! misses it is skipped before a handle is taken.

use crate::error::AppError;
use gdal::Dataset;

pub struct Footprint {
    west: f64,
    north: f64,
    /// Projected metres per mask cell.
    cell: f64,
    width: usize,
    height: usize,
    /// Row-major; true where the overview pixel has data.
    mask: Vec<bool>,
}

impl Footprint {
    /// `None` when the file has no overviews to build it from cheaply.
    pub fn build(ds: &Dataset) -> Result<Option<Self>, AppError> {
        let band = ds.rasterband(1)?;
        let count = band.overview_count()?;

        if count < 1 {
            return Ok(None);
        }

        let ov = band.overview(count as usize - 1)?;
        let (width, height) = ov.size();
        let nodata = band.no_data_value();

        let data = ov
            .read_as::<i32>((0, 0), (width, height), (width, height), None)?
            .into_shape_and_vec()
            .1;

        let gt = ds.geo_transform()?;

        Ok(Some(Self {
            west: gt[0],
            north: gt[3],
            cell: gt[1] * ds.raster_size().0 as f64 / width as f64,
            width,
            height,
            mask: data.iter().map(|&v| nodata != Some(v as f64)).collect(),
        }))
    }

    /// Whether any cell under the box has data. One cell of margin: an
    /// overview pixel averages its children, so a sliver of data can hide in
    /// a neighbour.
    pub fn covers(&self, (west, south, east, north): (f64, f64, f64, f64)) -> bool {
        let col = |x: f64| ((x - self.west) / self.cell).floor() as isize;
        let row = |y: f64| ((self.north - y) / self.cell).floor() as isize;

        let c0 = (col(west) - 1).max(0);
        let c1 = (col(east) + 1).min(self.width as isize - 1);
        let r0 = (row(north) - 1).max(0);
        let r1 = (row(south) + 1).min(self.height as isize - 1);

        (r0..=r1).any(|r| (c0..=c1).any(|c| self.mask[r as usize * self.width + c as usize]))
    }
}
