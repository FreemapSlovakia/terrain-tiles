# terrain-tiles

Elevation and hillshading tiles for Freemap, mosaicked on request from
per-country DEMs that are already in Web Mercator.

## Sources

One Cloud-Optimized GeoTIFF per country or region:

- EPSG:3857, on the XYZ pixel grid of its zoom (see below)
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

### Which zoom

A z17 pixel is 0.73–0.83 m on the ground at 46–52° N, a z18 one half that.

- Rendered from lidar points, or finer than 1 m: z18.
- A 1 m raster: z17 by default. z18 is a little sharper, since 1 m does not
  land on z17's grid and resampling that close softens it, but costs about
  4× the disk. Where a 1 m source sits at z18, it is the first to go to z17
  when space runs short.
- Coarser than 1 m: the zoom whose pixel is just finer than the data.

Every source can be rebuilt from its original at another zoom; nothing here is
final.

| Key | Zoom | Built from |
| --- | --- | --- |
| `sk` | z18 | DMR 5.0, 1 m raster in Krovák (JTSK03) |
| `cz` | z18 | DMR 5G points, gridded at 1 m in Krovák, then warped |
| `at` | z18 | ALS DTM, 1 m raster (EPSG:3035) |
| `ch` | z18 | swissALTI3D, 0.5 m (EPSG:2056) |
| `si` | z17 | DMR, 1 m raster (EPSG:3794) |
| `hr` | z17 | DMR, 1 m raster (EPSG:3765) |
| `pl` | z17 | NMT, 1 m raster (EPSG:2180) |
| `it` | z15 | HR-DTM, 5 m raster (EPSG:6875); its pixel-doubled 10 m patches rebuilt and its 10 m ripple removed first |
| `gedtm30` | z13 | GEDTM30, 30 m, Europe |

## Running

```sh
terrain-tiles --elevation-sources /data/elevation-sources \
  --source sk=/data/sk.tif --source cz=/data/cz.tif
```

Sources are listed highest priority first. A tile takes each pixel from the
first source that has data there. `X-Attribution` names the sources that
filled a pixel of the tile as `s<key>`.

A key is the `name` of the
[elevation-sources](https://github.com/FreemapSlovakia/elevation-sources)
datasets it was built from, and is credited with their attributions; the
server refuses to start for a key no dataset names. A source built from a
single dataset may instead be keyed by its directory without the number
(`245-de_by` → `de_by`), which is how a German state is credited alone when
all of them are named `de`.

## Routes

### `GET /licenses`

`{"shading:<key>": [{"title": …, "url": …}], …}` for every source, as the
outdoor renderer's `/licenses` keys its own, so a client resolves
`X-Attribution: s<key>` through either the same way. Revalidated on every use
(`no-cache`, `ETag`).

### Tiles

Both tile routes serve the tile plus a 2 px buffer's worth of data from its
neighbours.

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
