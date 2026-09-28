#!/usr/bin/env bash
# Warps one DTM into a source for terrain-tiles: EPSG:3857 on the XYZ pixel
# grid of ZOOM, Int32 heights in 2 mm steps, 256 px ZSTD blocks, overviews
# down to z8. CT_FILE holds the PROJ pipeline from SRS to EPSG:3857; the
# optional bbox (EPSG:3857 metres) limits the build. The extent is snapped
# outward to whole z8 tiles so every overview lands on the tile grid.
#
#   build-source.sh OUT.tif SRC SRS CT_FILE ZOOM [WEST SOUTH EAST NORTH]

set -euo pipefail

out=$1 src=$2 srs=$3 ct=$(<"$4") zoom=$5
shift 5

# Pilots set MIN_ZOOM higher so a small bbox is not padded to a z8 tile.
min_zoom=${MIN_ZOOM:-8}
levels=$((zoom - min_zoom))
tmp=${out%.tif}.tmp

read -r px te < <(python3 - "$zoom" "$min_zoom" "$src" "$srs" "$ct" "$tmp" "$@" <<'EOF'
import json, math, subprocess, sys

zoom, min_zoom, src, srs, ct, tmp = int(sys.argv[1]), int(sys.argv[2]), *sys.argv[3:7]
R = 20037508.342789244
px = 2 * R / 256 / 2**zoom
q = 2 * R / 2**min_zoom  # one z8 tile, metres

if len(sys.argv) > 7:
    w, s, e, n = map(float, sys.argv[7:11])
else:
    # A warped VRT computes only its extent here, not pixels.
    vrt = tmp + '.extent.vrt'
    subprocess.run(
        ['gdalwarp', '-q', '-overwrite', '-of', 'VRT', '-s_srs', srs, '-t_srs', 'EPSG:3857', '-ct', ct,
         '--config', 'GTIFF_SRS_SOURCE', 'EPSG', src, vrt], check=True)
    c = json.loads(subprocess.run(['gdalinfo', '-json', vrt],
                                  check=True, capture_output=True, text=True).stdout)['cornerCoordinates']
    w, n = c['upperLeft']
    e, s = c['lowerRight']

snap = lambda v, f: f((v + R) / q) * q - R
print(f'{px!r} {snap(w, math.floor)!r} {snap(s, math.floor)!r} {snap(e, math.ceil)!r} {snap(n, math.ceil)!r}')
EOF
)

echo "$(date -Is) $out: z$zoom, px $px, extent $te"

# Heights scaled to 2 mm steps before the warp, so the warp resamples in those
# units and writes integers directly (GDAL rounds). Source nodata stays nodata.
gdal_translate -q -of VRT -ot Float32 -scale 0 1 0 500 --config GTIFF_SRS_SOURCE EPSG \
  "$src" "$tmp.scaled.vrt"

gdalwarp -overwrite -s_srs "$srs" -t_srs EPSG:3857 -ct "$ct" \
  -te $te -tr "$px" "$px" -r cubic -ot Int32 -dstnodata -2147483648 \
  -wm 4096 -multi -wo NUM_THREADS=ALL_CPUS --config GDAL_CACHEMAX 4096 \
  -co TILED=YES -co BLOCKXSIZE=256 -co BLOCKYSIZE=256 -co BIGTIFF=YES -co SPARSE_OK=TRUE \
  -co COMPRESS=ZSTD -co ZSTD_LEVEL=9 -co PREDICTOR=2 -co NUM_THREADS=ALL_CPUS \
  "$tmp.scaled.vrt" "$tmp.tif"

gdal_edit.py -scale 0.002 -offset 0 "$tmp.tif"

echo "$(date -Is) $out: warped, building $levels overview levels"

factors=()
for ((i = 1; i <= levels; i++)); do factors+=($((1 << i))); done

gdaladdo -r average \
  --config COMPRESS_OVERVIEW ZSTD --config ZSTD_LEVEL_OVERVIEW 9 --config PREDICTOR_OVERVIEW 2 \
  --config BIGTIFF_OVERVIEW YES --config SPARSE_OK_OVERVIEW YES \
  --config GDAL_NUM_THREADS ALL_CPUS --config GDAL_CACHEMAX 4096 \
  "$tmp.tif" "${factors[@]}"

rm -f "$tmp.scaled.vrt" "$tmp.extent.vrt"
mv "$tmp.tif" "$out"

echo "$(date -Is) $out: done, $(du -h "$out" | cut -f1)"
