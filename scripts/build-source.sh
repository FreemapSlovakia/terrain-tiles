#!/usr/bin/env bash
# Warps one DTM into a source for terrain-tiles: EPSG:3857 on the XYZ pixel
# grid of ZOOM, Int32 heights in 2 mm steps, 256 px ZSTD blocks, overviews
# down to z8. CT_FILE holds the PROJ pipeline from SRS to EPSG:3857; the
# optional bbox (EPSG:3857 metres) limits the build. The extent is snapped
# outward to whole z8 tiles so every overview lands on the tile grid.
#
#   [PIECE_ZOOM=10] build-source.sh OUT.tif SRC SRS CT_FILE ZOOM [WEST SOUTH EAST NORTH]

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

co=(-co TILED=YES -co BLOCKXSIZE=256 -co BLOCKYSIZE=256 -co BIGTIFF=YES -co SPARSE_OK=TRUE
  -co COMPRESS=ZSTD -co ZSTD_LEVEL=9 -co PREDICTOR=2 -co NUM_THREADS=ALL_CPUS)
warp() {
  local te=$1 dst=$2
  shift 2
  gdalwarp -q -overwrite -s_srs "$srs" -t_srs EPSG:3857 -ct "$ct" \
    -te $te -tr "$px" "$px" -r cubic -ot Int32 -dstnodata -2147483648 \
    -wm 4096 -multi -wo NUM_THREADS=ALL_CPUS \
    --config GDAL_CACHEMAX 4096 --config GDAL_NUM_THREADS ALL_CPUS \
    "${co[@]}" "$@" "$dst"
}

if [ -z "${PIECE_ZOOM:-}" ]; then
  warp "$te" "$tmp.tif" "$tmp.scaled.vrt"
else
  # One warp over a source of many big compressed sheets decodes each sheet
  # again for every pass over its rows, once they no longer fit GDAL's cache;
  # PIECE_ZOOM tiles are warped one by one and joined. A mosaic VRT also
  # resamples sheets off its grid (nearest, rounding by request window), so
  # each piece warps the sheets it touches directly, each scaled alone.
  mkdir -p "$tmp.pieces" "$tmp.sheets"
  python3 - "$src" "$tmp.sheets" "$srs" <<'EOF'
import os, re, subprocess, sys
from concurrent.futures import ThreadPoolExecutor
from osgeo import gdal
gdal.UseExceptions()
src, d, srs = sys.argv[1:4]
xml = open(src).read()
base = os.path.dirname(os.path.abspath(src))
files = [f if r == '0' else os.path.join(base, f)
         for r, f in re.findall(r'<SourceFilename relativeToVRT="(\d)">([^<]+)<', xml)]
def sheet(i_f):
    i, f = i_f
    out = f'{d}/{i:05d}.vrt'
    if not os.path.exists(out):
        subprocess.run(['gdal_translate', '-q', '-of', 'VRT', '-ot', 'Float32', '-scale', '0', '1', '0', '500',
                        '-a_srs', srs, f, out], check=True)
    g = gdal.Open(f).GetGeoTransform()
    ds = gdal.Open(f)
    return f'{out} {g[0]} {g[3] + g[5] * ds.RasterYSize} {g[0] + g[1] * ds.RasterXSize} {g[3]}'
with ThreadPoolExecutor(8) as ex:
    lines = list(ex.map(sheet, enumerate(files)))
open(f'{d}/index', 'w').write('\n'.join(lines) + '\n')
EOF
  read -r w s e n <<< "$te"
  pq=$(python3 -c "print(2 * 20037508.342789244 / 2**$PIECE_ZOOM)")
  for x in $(python3 -c "import math; print(*range(math.ceil(($e - $w) / $pq)))"); do
    for y in $(python3 -c "import math; print(*range(math.ceil(($n - $s) / $pq)))"); do
      p="$tmp.pieces/${x}_$y.tif"
      [ -f "$p" ] && continue
      # The piece's extent, then the sheets within 100 m of it in the source CRS.
      mapfile -t args < <(python3 - "$tmp.sheets/index" "$srs" <<EOF
import sys
from osgeo import osr
w, s, e, n, q = $w, $s, $e, $n, $pq
pw, ps, pe, pn = w + $x * q, max(s, n - ($y + 1) * q), min(e, w + ($x + 1) * q), n - $y * q
print(pw, ps, pe, pn)
a = osr.SpatialReference(); a.ImportFromEPSG(3857)
b = osr.SpatialReference(); b.SetFromUserInput(sys.argv[2])
for r in (a, b): r.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
t = osr.CoordinateTransformation(a, b)
k = 16
pts = [t.TransformPoint(pw + (pe - pw) * i / k, ps + (pn - ps) * j / k)[:2]
       for i in range(k + 1) for j in range(k + 1) if i in (0, k) or j in (0, k)]
x0, x1 = min(p[0] for p in pts) - 100, max(p[0] for p in pts) + 100
y0, y1 = min(p[1] for p in pts) - 100, max(p[1] for p in pts) + 100
for l in open(sys.argv[1]):
    f, fw, fs, fe, fn = l.split()
    if float(fw) < x1 and float(fe) > x0 and float(fs) < y1 and float(fn) > y0:
        print(f)
EOF
)
      if [ ${#args[@]} -gt 1 ]; then
        warp "${args[0]}" "$p.part.tif" "${args[@]:1}"
        mv "$p.part.tif" "$p"
      fi
    done
    echo "$(date -Is) $out: piece column $((x + 1)) warped"
  done
  gdalbuildvrt -q -overwrite "$tmp.pieces.vrt" "$tmp.pieces"/*.tif
  gdal_translate -q -projwin $w $n $e $s "${co[@]}" \
    --config GDAL_CACHEMAX 4096 --config GDAL_NUM_THREADS ALL_CPUS \
    "$tmp.pieces.vrt" "$tmp.tif"
  rm -r "$tmp.pieces" "$tmp.pieces.vrt" "$tmp.sheets"
fi

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
