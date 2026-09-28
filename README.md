# terrain-tiles

Elevation and hillshading tiles for Freemap, mosaicked on request from
per-country DEMs that are already in Web Mercator.

## Sources

One Cloud-Optimized GeoTIFF per country or region:

- EPSG:3857, on the XYZ pixel grid of its zoom (z18 for 1 m data, lower for
  coarser or northern sources)
- Int32 heights in 2 mm steps (band scale `0.002`), nodata `-2147483648`
- ZSTD with the integer predictor
- 256 px blocks and overviews aligned to XYZ tiles, so a block is a tile

```sh
gdal_translate -of COG heights_i32.tif cz.tif \
  -co TILING_SCHEME=GoogleMapsCompatible -co ZOOM_LEVEL=18 -co BLOCKSIZE=256 \
  -co ALIGNED_LEVELS=9 -co RESAMPLING=NEAREST -co OVERVIEW_RESAMPLING=AVERAGE \
  -co COMPRESS=ZSTD -co LEVEL=19 -co PREDICTOR=YES -co SPARSE_OK=TRUE -co BIGTIFF=YES
```

`RESAMPLING=NEAREST` assumes the input is already on the z18 grid; the warp
from the national CRS belongs in the step before, where it resamples once.

## Running

```sh
terrain-tiles --source sk=/data/sk.tif --source cz=/data/cz.tif
```

Sources are listed highest priority first. A tile takes each pixel from the
first source that has data there. `X-Attribution` names the sources that
filled a pixel of the tile as `s<key>`.

## Routes

Both serve the tile plus a 2 px buffer's worth of data from its neighbours.

### `GET /elevation/{z}/{x}/{y}`

zstd over:

| bytes | content |
| --- | --- |
| 4 | `f32` LE height step, metres: 2 mm at z18 and above, doubling per zoom below |
| 4 | `u32` LE side, pixels (260: 256 + 2 × 2 buffer) |
| side² × 4 | `i32` LE heights in steps, row-major, each row delta-coded from 0 |

Decode a row by wrapping cumulative sum. `i32::MIN` after decoding is no data.

### `GET /hillshade/{z}/{x}/{y}?shading=…&format=png|jpeg|webp`

`shading` is the web client's `serializeShading` string (default: its
default shading). The slope uses the ground pixel size at each row's latitude.
JPEG composites over white; PNG and WebP (lossy, quality 90) keep alpha.
