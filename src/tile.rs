/// Half the Web-Mercator world width, metres.
pub const HALF_WORLD: f64 = 20037508.342789244;

pub const TILE: usize = 256;

/// Pixel size at z0, projected metres.
pub const PX_Z0: f64 = 2.0 * HALF_WORLD / TILE as f64;

/// Pixels read beyond each tile edge, so a 3×3 normal at the edge sees its
/// neighbours; the parametric-shading client expects exactly two.
pub const BUFFER: usize = 2;

pub const SIZE: usize = TILE + 2 * BUFFER;

/// An XYZ tile plus its buffer.
#[derive(Clone, Copy)]
pub struct TileWindow {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileWindow {
    pub fn new(z: u8, x: u32, y: u32, max_zoom: u8) -> Option<Self> {
        let n = 1u32.checked_shl(z.into())?;

        (z <= max_zoom && x < n && y < n).then_some(Self { z, x, y })
    }

    /// Pixel size at this zoom, projected metres.
    pub fn px(&self) -> f64 {
        PX_Z0 / (1u64 << self.z) as f64
    }

    /// West, south, east, north of the buffered window.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let px = self.px();
        let tile = px * TILE as f64;
        let b = px * BUFFER as f64;
        let west = -HALF_WORLD + self.x as f64 * tile - b;
        let north = HALF_WORLD - self.y as f64 * tile + b;

        (
            west,
            north - SIZE as f64 * px,
            west + SIZE as f64 * px,
            north,
        )
    }

    /// Ground metres per pixel on window row `row`.
    pub fn ground_px(&self, row: usize) -> f64 {
        let px = self.px();
        let y = self.bounds().3 - (row as f64 + 0.5) * px;
        let lat = (y / HALF_WORLD * std::f64::consts::PI).sinh().atan();

        px * lat.cos()
    }
}
