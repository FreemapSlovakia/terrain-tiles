# terrain-tiles

Elevation and hillshading tiles for Freemap, mosaicked on request from
per-country DEMs that are already in Web Mercator.

## Sources

One tiled GeoTIFF per country or region:

- EPSG:3857, on the XYZ pixel grid of its zoom (see below)
- Int32 heights in 2 mm steps (band scale `0.002`), nodata `-2147483648`
- ZSTD with the integer predictor
- 256 px blocks and internal average overviews down to z8, aligned to XYZ
  tiles, so a block is a tile

[`scripts/build-source.sh`](scripts/build-source.sh) builds one from a
national DTM: it warps once, cubic, straight onto the grid, then adds the
overviews. The files are not COGs — the overviews and the main IFD follow the
data — which costs nothing read from local disk.

### Which zoom

A z17 pixel is 0.73–0.83 m on the ground at 46–52° N, a z18 one half that.
A pixel shrinks on the ground with the cosine of latitude, so the rules below
are for those latitudes: at 58–71° N a z16 pixel is already 0.8–1.25 m.

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
| `cz` | z18 | DMR 5G points by natural neighbours straight onto the z18 grid, gaps membrane-filled, clipped to the border + 500 m |
| `at` | z18 | ALS DTM, 1 m raster (EPSG:3035) |
| `ch` | z18 | swissALTI3D, 0.5 m (EPSG:2056) |
| `de_<state>` | z17 | DGM1 of each German state, 1 m rasters (EPSG:25832, or 25833 for Saxony, Mecklenburg-Vorpommern and Brandenburg), one source per state |
| `nl`, `be_vlg`, `be_wal`, `lu` | z17 | AHN5 DTM, DHMV II, SPW MNT 2021–22 and the 2024 LiDAR MNT, 1 m rasters (EPSG:28992, 3812, 3812, 2169) |
| `si` | z17 | DMR, 1 m raster (EPSG:3794) |
| `hr` | z17 | DMR, 1 m raster (EPSG:3765) |
| `pl` | z17 | NMT, 1 m raster (EPSG:2180) |
| `it` | z15 | HR-DTM, 5 m raster (EPSG:6875); its pixel-doubled 10 m patches rebuilt and its 10 m ripple removed first |
| `es_29`, `es_30`, `es_31` | z16 | MDT02, 2 m rasters, one source per UTM zone (EPSG:25829, 25830, 25831); the sheets lie on differing grids, so each is warped directly rather than through a mosaic |
| `fr` | z15 | RGE ALTI, 1 m raster (EPSG:2154), mainland; in the mountains it is ~5 m data on a 1 m grid |
| `en`, `wls` | z17 | LIDAR Composite DTM (England) and the Welsh Government LiDAR DTM, 1 m rasters (EPSG:27700, OSTN15 shift) |
| `no` | z16 | Kartverket DTM, 1 m raster (EPSG:25833) |
| `fi` | z16 | NLS elevation model, 2 m raster (EPSG:3067) |
| `dk` | z17 | Danmarks Højdemodel terrain, 1 m raster (EPSG:25832) |
| `se` | z16 | Lantmäteriet Markhöjdmodell, 1 m raster (EPSG:3006, whose N,E axis order needs the axisswap) |
| `gedtm30` | z13 | GEDTM30, 30 m, Europe |

## Running

```sh
terrain-tiles --elevation-sources /fm/storage1/backend.freemap.sk-data/elevation-sources
```

The sources are the
[elevation-sources](https://github.com/FreemapSlovakia/elevation-sources)
datasets that link a `mercator.tif`, ranked by their directory number, lowest
first. A tile takes each pixel from the first source that has data there.
Adding, removing or reordering a source is a change there, then a restart.

A source is keyed by its directory name without the number, `-` read as `_`
(`245-de_by` → `de_by`, `090-es-29` → `es_29`), and credited with its
`source.json` attributions. `X-Attribution` names the sources that filled a
pixel of the tile as `s<key>`.

## Routes

### `GET /licenses`

`{"shading:<key>": [{"title": …, "url": …}], …}` for every source, as the
outdoor renderer's `/licenses` keys its own, so a client resolves
`X-Attribution: s<key>` through either the same way. Revalidated on every use
(`no-cache`, `ETag`).

### Tiles

Both tile routes read the tile plus a buffer of data from its neighbours:
2 px unless asked for more.

### `GET /elevation/{z}/{x}/{y}?buffer=2…64`

zstd over:

| bytes | content |
| --- | --- |
| 4 | `f32` LE height step, metres: 2 mm at z18 and above, doubling per zoom below |
| 4 | `u32` LE side, pixels: 256 + 2 × `buffer` |
| side² × 4 | `i32` LE heights in steps, row-major, each row delta-coded from 0 |

Decode a row by wrapping cumulative sum. `i32::MIN` after decoding is no data.

### `GET /hillshade/{z}/{x}/{y}?shading=…&format=png|jpeg|webp`

`shading` is the web client's `serializeShading` string (default: its
default shading); the buffer is the widest its layers read. The slope uses the
ground pixel size at each row's latitude.
JPEG composites over white; PNG and WebP (lossy, quality 90) keep alpha.
