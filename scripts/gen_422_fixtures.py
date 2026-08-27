#!/usr/bin/env python3
"""Generate the 4:2:2 HEVC fixtures under testdata/hevc422/ and testdata/features/.

Two independent oracles, no reference decoder needed at test time:

  * x265 `--lossless` raw Annex-B streams (`testdata/hevc422/*.hevc`) coded
    straight from a synthetic i422 YUV source (`*.yuv`, committed next to the
    stream). A correct decode reproduces the source planes bit-exactly, so the
    test compares against the source itself.
  * heif-enc (libheif + x265, `-p chroma=422`) HEIC files
    (`testdata/features/yuv422*.heic`) plus libde265's `dec265` decode of the
    coded bitstream, written as raw planar YUV (`testdata/hevc422/*.ref.yuv`,
    same layout as the x265 sources). Lossy, so this exercises
    dequant/transform, the ChromaArrayType != 1 chroma QP table, 4:2:2
    deblocking geometry and SAO — libde265 is the reference.

Requires: python3, x265, heif-enc on PATH, and dec265 (libde265 built from
source: `git clone https://github.com/strukturag/libde265 && cmake && make
dec265`; point DEC265 at the binary). No numpy / ImageMagick.
The synthetic image is generated deterministically here (no external inputs),
so re-running regenerates byte-identical sources; encoder output may differ
across x265 versions, in which case the committed reference must be
regenerated with the same tool that produced the stream.
"""

import os
import shutil
import struct
import subprocess
import sys
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RAW_DIR = os.path.join(ROOT, "testdata", "hevc422")
FEAT_DIR = os.path.join(ROOT, "testdata", "features")

W, H = 80, 56  # multiple of 8 (MinCbSize), NOT a multiple of the CTU size

# libde265's reference decoder (built from source; see docstring). Override
# with DEC265=/path/to/dec265.
DEC265 = os.environ.get(
    "DEC265", os.path.join(os.path.expanduser("~"), "tmp", "libde265-src", "build", "dec265", "dec265")
)


def lcg(seed):
    """Deterministic 32-bit LCG (Numerical Recipes constants)."""
    state = seed & 0xFFFFFFFF
    while True:
        state = (state * 1664525 + 1013904223) & 0xFFFFFFFF
        yield state >> 16


def synth_rgb(w, h, maxv):
    """Synthetic RGB image with gradients, hard edges, noise, and stripes.

    Returns three row-major planes of ints in 0..=maxv.
    """
    rnd = lcg(0x5EED)
    r_p, g_p, b_p = [], [], []
    for y in range(h):
        for x in range(w):
            # base: horizontal luma ramp, vertical hue ramp
            r = x / (w - 1)
            g = y / (h - 1)
            b = 1.0 - r * 0.5 - g * 0.5
            # saturated hard-edged square (strong chroma edge)
            if 12 <= x < 34 and 10 <= y < 30:
                r, g, b = 0.95, 0.10, 0.15
            # diagonal chroma stripes (high-frequency chroma)
            if 40 <= x < 76 and 6 <= y < 26 and ((x + y) // 3) % 2 == 0:
                r, g, b = 0.1, 0.2, 0.9
            # noise region (defeats smooth prediction)
            if 6 <= x < 38 and 34 <= y < 52:
                n = next(rnd)
                r = ((n >> 0) & 0xFF) / 255.0
                g = ((n >> 4) & 0xFF) / 255.0
                b = ((n >> 8) & 0xFF) / 255.0
            # thin dark/light checker (text-like)
            if 46 <= x < 74 and 32 <= y < 50 and ((x // 2) + (y // 2)) % 2 == 0:
                r, g, b = 0.05, 0.05, 0.05
            r_p.append(round(r * maxv))
            g_p.append(round(g * maxv))
            b_p.append(round(b * maxv))
    return r_p, g_p, b_p


def rgb_to_ycbcr(r, g, b, maxv):
    """Full-range BT.601 (only used to derive the x265 raw source; any
    consistent mapping works since the test compares YUV against YUV)."""
    y = 0.299 * r + 0.587 * g + 0.114 * b
    cb = (b - y) / 1.772 + (maxv + 1) / 2
    cr = (r - y) / 1.402 + (maxv + 1) / 2
    clamp = lambda v: max(0, min(maxv, int(round(v))))
    return clamp(y), clamp(cb), clamp(cr)


def write_i422(path, w, h, depth):
    maxv = (1 << depth) - 1
    r_p, g_p, b_p = synth_rgb(w, h, maxv)
    y_plane, cb_full, cr_full = [], [], []
    for i in range(w * h):
        y, cb, cr = rgb_to_ycbcr(r_p[i], g_p[i], b_p[i], maxv)
        y_plane.append(y)
        cb_full.append(cb)
        cr_full.append(cr)
    # 4:2:2 — average horizontal pairs, full vertical resolution
    cb_p, cr_p = [], []
    for y in range(h):
        for x in range(0, w, 2):
            i = y * w + x
            cb_p.append((cb_full[i] + cb_full[i + 1] + 1) >> 1)
            cr_p.append((cr_full[i] + cr_full[i + 1] + 1) >> 1)
    fmt = "<%dB" if depth == 8 else "<%dH"
    with open(path, "wb") as f:
        for plane in (y_plane, cb_p, cr_p):
            f.write(struct.pack(fmt % len(plane), *plane))


def write_png(path, w, h, depth):
    maxv = (1 << depth) - 1
    r_p, g_p, b_p = synth_rgb(w, h, maxv)
    raw = bytearray()
    for y in range(h):
        raw.append(0)  # filter type none
        for x in range(w):
            i = y * w + x
            if depth == 8:
                raw += struct.pack(">BBB", r_p[i], g_p[i], b_p[i])
            else:
                raw += struct.pack(">HHH", r_p[i], g_p[i], b_p[i])

    def chunk(typ, data):
        c = struct.pack(">I", len(data)) + typ + data
        return c + struct.pack(">I", zlib.crc32(typ + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", w, h, depth, 2, 0, 0, 0)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", zlib.compress(bytes(raw), 9)))
        f.write(chunk(b"IEND", b""))


def run(cmd):
    print("  $", " ".join(cmd))
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def heic_to_annexb(path):
    """Re-frame a single-image HEIC's HEVC payload as an Annex-B byte stream.

    hvcC's NAL arrays (VPS/SPS/PPS) come first, then the item extent's
    length-prefixed slice NAL units, each with a 00 00 00 01 start code.
    """
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    from gen_feature_fixtures import extract  # box walker + iloc/hvcC extractor

    hvcc, _colr, extent = extract(path)
    cfg = hvcc[8:]  # strip the 8-byte box header
    length_size = (cfg[21] & 3) + 1
    out = bytearray()
    q = 23
    for _ in range(cfg[22]):  # numOfArrays
        num_nalus = struct.unpack(">H", cfg[q + 1:q + 3])[0]
        q += 3
        for _ in range(num_nalus):
            n = struct.unpack(">H", cfg[q:q + 2])[0]
            out += b"\x00\x00\x00\x01" + cfg[q + 2:q + 2 + n]
            q += 2 + n
    p = 0
    while p < len(extent):
        n = int.from_bytes(extent[p:p + length_size], "big")
        out += b"\x00\x00\x00\x01" + extent[p + length_size:p + length_size + n]
        p += length_size + n
    return bytes(out)


def main():
    for tool in ("x265", "heif-enc"):
        if shutil.which(tool) is None:
            sys.exit(f"{tool} not found on PATH")
    if not os.access(DEC265, os.X_OK):
        sys.exit(f"dec265 not found at {DEC265} (set DEC265=...)")
    os.makedirs(RAW_DIR, exist_ok=True)
    os.makedirs(FEAT_DIR, exist_ok=True)
    tmp = os.path.join(os.path.expanduser("~"), "tmp", "heic422-gen")
    os.makedirs(tmp, exist_ok=True)

    # ---- x265 lossless raw streams (source-exact oracle) --------------------
    # x265 requires the picture to be at least one CTU (56 < 64), so 64 is out;
    # the max TU depth is log2(ctu) - 2 + 1 (4 for a 32 CTU, 3 for 16).
    for depth, ctu, tu_depth in ((8, "32", "4"), (10, "16", "3")):
        src = os.path.join(RAW_DIR, f"src_422_{depth}bit_{W}x{H}.yuv")
        write_i422(src, W, H, depth)
        out = os.path.join(RAW_DIR, f"x265_422_{depth}bit_lossless.hevc")
        run([
            "x265", "--input", src, "--input-res", f"{W}x{H}", "--input-csp", "i422",
            "--input-depth", str(depth), "--output-depth", str(depth), "--fps", "1",
            "--frames", "1", "--lossless", "--preset", "slow", "--ctu", ctu,
            "--tu-intra-depth", tu_depth, "--tu-inter-depth", tu_depth,
            "--rect", "--amp", "--no-open-gop", "-o", out,
        ])

    # ---- heif-enc HEIC + heif-dec (libde265) reference -----------------------
    png8 = os.path.join(tmp, "rgb8.png")
    png16 = os.path.join(tmp, "rgb16.png")
    write_png(png8, W, H, 8)
    write_png(png16, W, H, 16)
    for name, png, args in (
        ("yuv422", png8, ["-q", "35"]),  # low quality → high QP → chroma-QP table path
        ("yuv422_10bit", png16, ["-q", "60", "-b", "10"]),
    ):
        heic = os.path.join(FEAT_DIR, f"{name}.heic")
        run(["heif-enc", *args, "-p", "chroma=422", "-p", "preset=slow",
             "-p", "tu-intra-depth=4", png, "-o", heic])
        # heif-dec's y4m/png outputs are colour-converted (4:2:0 / RGB), so the
        # reference is dec265 on the coded bitstream itself: hvcC parameter
        # sets + the length-prefixed extent, re-framed as Annex B.
        annexb = os.path.join(tmp, f"{name}.hevc")
        with open(annexb, "wb") as f:
            f.write(heic_to_annexb(heic))
        ref = os.path.join(RAW_DIR, f"{name}.ref.yuv")
        run([DEC265, "-q", "-o", ref, annexb])

    for d in (RAW_DIR, FEAT_DIR):
        for f in sorted(os.listdir(d)):
            if "422" in f:
                print(f"{os.path.getsize(os.path.join(d, f)):7d}  {os.path.join(d, f)}")


if __name__ == "__main__":
    main()
