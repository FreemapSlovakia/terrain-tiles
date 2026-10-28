/// Half the Web-Mercator world width, metres.
pub const HALF_WORLD: f64 = 20037508.342789244;

pub const TILE: usize = 256;

/// Pixel size at z0, projected metres.
pub const PX_Z0: f64 = 2.0 * HALF_WORLD / TILE as f64;

/// Pixels read beyond each tile edge unless more are asked for, so a 3×3
/// normal at the edge sees its neighbours; the parametric-shading client
/// expects exactly two.
pub const BUFFER: usize = 2;

/// The widest buffer a request may ask for.
pub const MAX_BUFFER: usize = 64;

/// Ground metres per pixel along the middle of tile row `y` at zoom `z`.
pub fn centre_ground_px(z: u8, y: u32) -> f64 {
    let px = PX_Z0 / (1u64 << z) as f64;
    let merc_y = HALF_WORLD - (y as f64 + 0.5) * TILE as f64 * px;
    let lat = (merc_y / HALF_WORLD * std::f64::consts::PI).sinh().atan();

    px * lat.cos()
}

/// An XYZ tile plus its buffer.
#[derive(Clone, Copy)]
pub struct TileWindow {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub buffer: usize,
}

impl TileWindow {
    pub fn new(z: u8, x: u32, y: u32, max_zoom: u8) -> Option<Self> {
        let n = 1u32.checked_shl(z.into())?;

        (z <= max_zoom && x < n && y < n).then_some(Self {
            z,
            x,
            y,
            buffer: BUFFER,
        })
    }

    pub fn with_buffer(self, buffer: usize) -> Self {
        Self { buffer, ..self }
    }

    /// Side of the buffered window, pixels.
    pub fn size(&self) -> usize {
        TILE + 2 * self.buffer
    }

    /// Pixel size at this zoom, projected metres.
    pub fn px(&self) -> f64 {
        PX_Z0 / (1u64 << self.z) as f64
    }

    /// West, south, east, north of the buffered window.
    pub fn bounds(&self) -> (f64, f64, f64, f64) {
        let px = self.px();
        let tile = px * TILE as f64;
        let b = px * self.buffer as f64;
        let west = -HALF_WORLD + self.x as f64 * tile - b;
        let north = HALF_WORLD - self.y as f64 * tile + b;
        let size = self.size() as f64 * px;

        (west, north - size, west + size, north)
    }

    /// Ground metres per pixel on window row `row`.
    pub fn ground_px(&self, row: usize) -> f64 {
        let px = self.px();
        let y = self.bounds().3 - (row as f64 + 0.5) * px;
        let lat = (y / HALF_WORLD * std::f64::consts::PI).sinh().atan();

        px * lat.cos()
    }
}
