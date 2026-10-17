#!/usr/bin/env python3
"""Generate the small per-feature HEIF fixtures under testdata/features/.

Every fixture is tiny (<10 KB) and exercises one container/decode feature path
that the synthetic and uncompressed corpora don't reach: derived images
(grid / iden / iovl), item-property transforms (irot / imir / clap), auxiliary
alpha, thumbnails, monochrome (4:0:0), 10-bit, nclx colour signalling, and
EXIF / XMP metadata items.

Provenance / reproducibility — this is the ONLY source of these fixtures. They
are regenerated, not hand-edited. Re-run after a tooling bump and re-verify with
`cargo test --test cov_features`.

Requires (dev box only; not needed in CI — the fixtures are committed):
  * heif-enc (libheif >= 1.21 with x265)   — base coded images, grid, alpha, thumb
  * ImageMagick `convert`                   — source PNGs
  * exiftool                                — EXIF / XMP injection

The derived-item (iden/iovl) and property (irot/imir) fixtures are built by a
minimal ISOBMFF muxer below — libheif/heif-enc can't emit those. Each generated
file is cross-checked against libheif `heif-dec` in the test suite, so a
malformed mux would be caught (see tests/cov_features.rs).
"""
import os, struct, subprocess, sys, shutil

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "testdata", "features")
LIBHEIF = "/home/lilith/work/heic/libheif/build-native"
HEIFENC = os.path.join(LIBHEIF, "examples", "heif-enc")
ENV = dict(os.environ, LD_LIBRARY_PATH=f"{LIBHEIF}:{LIBHEIF}/libheif:" + os.environ.get("LD_LIBRARY_PATH", ""))


def run(cmd, **kw):
    subprocess.run(cmd, check=True, env=ENV, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, **kw)


def png(path, *magick_args):
    run(["convert", *magick_args, path])


def enc(src, dst, *args):
    run([HEIFENC, "-q", "80", *args, src, "-o", dst])


# ---- minimal ISOBMFF helpers ------------------------------------------------
def box(typ, payload):
    return struct.pack(">I", 8 + len(payload)) + typ + payload


def fbox(typ, ver, flags, payload):
    return box(typ, bytes([ver]) + flags.to_bytes(3, "big") + payload)


def ispe(w, h):
    return fbox(b"ispe", 0, 0, struct.pack(">II", w, h))


def irot(a):
    return box(b"irot", bytes([a & 3]))


def imir(m):
    return box(b"imir", bytes([m & 1]))


def _children(b, off, end, acc):
    while off + 8 <= end:
        sz = struct.unpack(">I", b[off:off + 4])[0]
        t = b[off + 4:off + 8]
        hs = 8
        if sz == 1:
            sz = struct.unpack(">Q", b[off + 8:off + 16])[0]
            hs = 16
        acc.append((t, off, sz, hs))
        if t in (b"meta", b"iprp", b"ipco"):
            _children(b, off + hs + (4 if t == b"meta" else 0), off + sz, acc)
        off += sz


def extract(path):
    """Pull the coded HEVC extent + hvcC + colr from a heif-enc single image."""
    b = open(path, "rb").read()
    acc = []
    _children(b, 0, len(b), acc)
    g = lambda t: [x for x in acc if x[0] == t][0]
    _, o, s, _ = g(b"hvcC"); hvcc = b[o:o + s]
    _, o, s, _ = g(b"colr"); colr = b[o:o + s]
    lo = g(b"iloc")[1]
    q = lo + 12
    osz, lsz, bsz = b[q] >> 4, b[q] & 0xF, b[q + 1] >> 4
    q += 2
    struct.unpack(">H", b[q:q + 2]); q += 2  # item_count
    q += 2  # item_id
    if b[lo + 8] >= 1:
        q += 2
    q += 2  # data_ref_index
    base = int.from_bytes(b[q:q + bsz], "big"); q += bsz
    q += 2  # extent_count
    off = int.from_bytes(b[q:q + osz], "big"); q += osz
    length = int.from_bytes(b[q:q + lsz], "big")
    return hvcc, colr, b[base + off:base + off + length]


# ---- property injection (irot/imir) -----------------------------------------
def inject(src, dst, props, essential=1):
    b = bytearray(open(src, "rb").read())
    acc = []
    _children(b, 0, len(b), acc)
    g = lambda t: [x for x in acc if x[0] == t]
    mo, msz, _ = g(b"meta")[0][1], g(b"meta")[0][2], 0
    meta = g(b"meta")[0]; mo, msz = meta[1], meta[2]
    iprp = g(b"iprp")[0]; io, isz = iprp[1], iprp[2]
    ipco = g(b"ipco")[0]; co, csz = ipco[1], ipco[2]
    nprops = sum(1 for t, o, s, h in acc
                 if co + 8 <= o < co + csz and t not in (b"ipco",))
    # count ipco direct children
    nprops = 0; o = co + 8
    while o + 8 < co + csz:
        sz = struct.unpack(">I", b[o:o + 4])[0]; nprops += 1; o += sz
    newprops = b"".join(props)
    new_idx = [nprops + 1 + i for i in range(len(props))]
    po = g(b"ipma")[0][1]
    ver = b[po + 8]; flags = int.from_bytes(b[po + 9:po + 12], "big")
    q = po + 12; ec = struct.unpack(">I", b[q:q + 4])[0]; q += 4
    out = bytearray(b[po + 8:q])
    for ei in range(ec):
        if ver < 1:
            itembytes = b[q:q + 2]; q += 2
        else:
            itembytes = b[q:q + 4]; q += 4
        ac = b[q]; q += 1
        assoc = bytearray()
        for _ in range(ac):
            n = 2 if flags & 1 else 1
            assoc += b[q:q + n]; q += n
        if ei == 0:
            for idx in new_idx:
                assoc.append((essential << 7) | idx); ac += 1
        out += itembytes + bytes([ac]) + assoc
    ipma_new = struct.pack(">I", 8 + len(out)) + b"ipma" + out
    ipco_new = struct.pack(">I", csz + len(newprops)) + b"ipco" + bytes(b[co + 8:co + csz]) + newprops
    iprp_new = struct.pack(">I", 8 + len(ipco_new) + len(ipma_new)) + b"iprp" + ipco_new + ipma_new
    meta_inner = bytes(b[mo + 8:io]) + iprp_new + bytes(b[io + isz:mo + msz])
    meta_new = struct.pack(">I", 8 + len(meta_inner)) + b"meta" + meta_inner
    delta = len(meta_new) - msz
    out_file = bytearray(b[:mo]) + meta_new + bytes(b[mo + msz:])
    # bump absolute iloc base offsets by delta (meta grew, mdat shifted)
    acc2 = []; _children(out_file, 0, len(out_file), acc2)
    nlo = [x for x in acc2 if x[0] == b"iloc"][0][1]
    ver_l = out_file[nlo + 8]; q = nlo + 12
    osz = out_file[q] >> 4; lsz = out_file[q] & 0xF; bsz = out_file[q + 1] >> 4; q += 2
    cnt = struct.unpack(">H", out_file[q:q + 2])[0]; q += 2
    for _ in range(cnt):
        q += 2
        if ver_l >= 1:
            q += 2
        q += 2
        if bsz:
            v = int.from_bytes(out_file[q:q + bsz], "big")
            out_file[q:q + bsz] = (v + delta).to_bytes(bsz, "big"); q += bsz
        ecnt = struct.unpack(">H", out_file[q:q + 2])[0]; q += 2
        for _ in range(ecnt):
            if bsz == 0:
                v = int.from_bytes(out_file[q:q + osz], "big")
                out_file[q:q + osz] = (v + delta).to_bytes(osz, "big")
            q += osz + lsz
    open(dst, "wb").write(out_file)


# ---- derived-item muxer (iden / iovl) ---------------------------------------
def mux_derived(dst, base_w, base_h, kind, out_w, out_h, src,
                irot_angle=None, overlay_offset=None):
    hvcc, colr, coded = extract(src)
    props = [hvcc, ispe(base_w, base_h), colr, ispe(out_w, out_h)]
    deriv = [(0, 4)]
    if irot_angle is not None:
        props.append(irot(irot_angle)); deriv.append((1, len(props)))
    ipco = box(b"ipco", b"".join(props))

    def item_assoc(item, lst):
        r = struct.pack(">H", item) + bytes([len(lst)])
        for e, i in lst:
            r += bytes([(e << 7) | i])
        return r

    ipma = fbox(b"ipma", 0, 0, struct.pack(">I", 2)
                + item_assoc(1, [(1, 1), (0, 2), (0, 3)]) + item_assoc(2, deriv))
    iprp = box(b"iprp", ipco + ipma)

    def infe(item, typ, hidden):
        return fbox(b"infe", 2, 1 if hidden else 0,
                    struct.pack(">HH", item, 0) + typ + b"drv\x00")

    iinf = fbox(b"iinf", 0, 0, struct.pack(">H", 2)
                + infe(1, b"hvc1", True) + infe(2, kind, False))
    dimg = box(b"dimg", struct.pack(">HH", 2, 1) + struct.pack(">H", 1))
    iref = fbox(b"iref", 0, 0, dimg)
    pitm = fbox(b"pitm", 0, 0, struct.pack(">H", 2))
    hdlr = fbox(b"hdlr", 0, 0, struct.pack(">I", 0) + b"pict" + b"\x00" * 12)
    item2 = b""
    if kind == b"iovl":
        ho, vo = overlay_offset
        item2 = bytes([0, 0]) + struct.pack(">HHHH", 0, 0, 0, 65535) \
            + struct.pack(">HH", out_w, out_h) + struct.pack(">hh", ho, vo)
    mdat_payload = coded + item2
    ftyp = box(b"ftyp", b"heic" + struct.pack(">I", 0) + b"mif1" + b"heic")

    def iloc(o1, o2):
        body = bytes([0x44, 0x00]) + struct.pack(">H", 2)
        body += struct.pack(">HHH", 1, 0, 1) + struct.pack(">II", o1, len(coded))
        if kind == b"iovl":
            body += struct.pack(">HHH", 2, 0, 1) + struct.pack(">II", o2, len(item2))
        else:
            body += struct.pack(">HHH", 2, 0, 0)
        return fbox(b"iloc", 0, 0, body)

    o1 = o2 = 0
    for _ in range(4):
        meta = fbox(b"meta", 0, 0, hdlr + pitm + iinf + iref + iprp + iloc(o1, o2))
        start = len(ftyp) + len(meta) + 8
        o1, o2 = start, start + len(coded)
    open(dst, "wb").write(ftyp + meta + box(b"mdat", mdat_payload))


# ---- edge-case overlay / iden-chain (malformed + limit + 32-bit paths) -------
def mux_overlay(dst, canvas_w, canvas_h, offset, version=0, large=False, src=None):
    """Overlay with full control: version (reject path), flags&1 (32-bit), signed
    offsets (negative-offset src_skip), oversized canvas (limit rejection)."""
    hvcc, colr, coded = extract(src)
    ew = canvas_w if canvas_w < 65536 else 65535
    eh = canvas_h if canvas_h < 65536 else 65535
    ipco = box(b"ipco", b"".join([hvcc, ispe(64, 64), colr, ispe(ew, eh)]))

    def item_assoc(item, lst):
        r = struct.pack(">H", item) + bytes([len(lst)])
        for e, i in lst:
            r += bytes([(e << 7) | i])
        return r

    ipma = fbox(b"ipma", 0, 0, struct.pack(">I", 2)
                + item_assoc(1, [(1, 1), (0, 2), (0, 3)]) + item_assoc(2, [(0, 4)]))
    iprp = box(b"iprp", ipco + ipma)

    def infe(item, typ, hidden):
        return fbox(b"infe", 2, 1 if hidden else 0,
                    struct.pack(">HH", item, 0) + typ + b"drv\x00")

    iinf = fbox(b"iinf", 0, 0, struct.pack(">H", 2)
                + infe(1, b"hvc1", True) + infe(2, b"iovl", False))
    iref = fbox(b"iref", 0, 0, box(b"dimg", struct.pack(">HHH", 2, 1, 1)))
    pitm = fbox(b"pitm", 0, 0, struct.pack(">H", 2))
    hdlr = fbox(b"hdlr", 0, 0, struct.pack(">I", 0) + b"pict" + b"\x00" * 12)
    flags = 1 if large else 0
    ho, vo = offset
    fill = struct.pack(">HHHH", 0, 0, 0, 65535)
    if large:
        item2 = bytes([version, flags]) + fill \
            + struct.pack(">II", canvas_w, canvas_h) + struct.pack(">ii", ho, vo)
    else:
        item2 = bytes([version, flags]) + fill \
            + struct.pack(">HH", canvas_w, canvas_h) + struct.pack(">hh", ho, vo)
    ftyp = box(b"ftyp", b"heic" + struct.pack(">I", 0) + b"mif1" + b"heic")

    def iloc(o1, o2):
        body = bytes([0x44, 0x00]) + struct.pack(">H", 2)
        body += struct.pack(">HHH", 1, 0, 1) + struct.pack(">II", o1, len(coded))
        body += struct.pack(">HHH", 2, 0, 1) + struct.pack(">II", o2, len(item2))
        return fbox(b"iloc", 0, 0, body)

    o1 = o2 = 0
    for _ in range(4):
        meta = fbox(b"meta", 0, 0, hdlr + pitm + iinf + iref + iprp + iloc(o1, o2))
        st = len(ftyp) + len(meta) + 8
        o1, o2 = st, st + len(coded)
    open(dst, "wb").write(ftyp + meta + box(b"mdat", coded + item2))


def mux_iden_chain(dst, n, src):
    """n-deep iden->iden->...->coded chain. n>MAX_DERIVED_DEPTH(3) must reject."""
    hvcc, colr, coded = extract(src)
    ipco = box(b"ipco", b"".join([hvcc, ispe(64, 64), colr]))

    def item_assoc(item, lst):
        r = struct.pack(">H", item) + bytes([len(lst)])
        for e, i in lst:
            r += bytes([(e << 7) | i])
        return r

    entries = [item_assoc(1, [(1, 1), (0, 2), (0, 3)])]
    entries += [item_assoc(it, [(0, 2)]) for it in range(2, 2 + n)]
    ipma = fbox(b"ipma", 0, 0, struct.pack(">I", 1 + n) + b"".join(entries))
    iprp = box(b"iprp", ipco + ipma)

    def infe(item, typ, hidden):
        return fbox(b"infe", 2, 1 if hidden else 0,
                    struct.pack(">HH", item, 0) + typ + b"d\x00")

    infes = [infe(1, b"hvc1", True)]
    infes += [infe(it, b"iden", True) for it in range(2, 1 + n)]
    infes += [infe(1 + n, b"iden", False)]
    iinf = fbox(b"iinf", 0, 0, struct.pack(">H", 1 + n) + b"".join(infes))
    drefs = b"".join(box(b"dimg", struct.pack(">HHH", it, 1, it - 1))
                     for it in range(2, 2 + n))
    iref = fbox(b"iref", 0, 0, drefs)
    pitm = fbox(b"pitm", 0, 0, struct.pack(">H", 1 + n))
    hdlr = fbox(b"hdlr", 0, 0, struct.pack(">I", 0) + b"pict" + b"\x00" * 12)
    ftyp = box(b"ftyp", b"heic" + struct.pack(">I", 0) + b"mif1" + b"heic")

    def iloc(o1):
        body = bytes([0x44, 0x00]) + struct.pack(">H", 1 + n)
        body += struct.pack(">HHH", 1, 0, 1) + struct.pack(">II", o1, len(coded))
        for it in range(2, 2 + n):
            body += struct.pack(">HHH", it, 0, 0)
        return fbox(b"iloc", 0, 0, body)

    o1 = 0
    for _ in range(4):
        meta = fbox(b"meta", 0, 0, hdlr + pitm + iinf + iref + iprp + iloc(o1))
        o1 = len(ftyp) + len(meta) + 8
    open(dst, "wb").write(ftyp + meta + box(b"mdat", coded))


# ---- generate ---------------------------------------------------------------
def main():
    if not os.path.exists(HEIFENC):
        sys.exit(f"heif-enc not found at {HEIFENC} — install libheif build")
    os.makedirs(OUT, exist_ok=True)
    tmp = "/tmp/featgen"
    os.makedirs(tmp, exist_ok=True)

    # source PNGs
    png(f"{tmp}/rgb.png", "-size", "48x48", "gradient:red-blue")
    png(f"{tmp}/rgba.png", "-size", "48x48", "gradient:red-blue", "-alpha", "set",
        "-channel", "A", "-evaluate", "set", "50%")
    png(f"{tmp}/gray.png", "-size", "48x48", "gradient:black-white", "-colorspace", "Gray")
    png(f"{tmp}/rgb16.png", "-size", "64x64", "gradient:red-blue", "-depth", "16")
    png(f"{tmp}/plasma.png", "-size", "96x96", "plasma:fractal")
    # corner-marked bases (TL=red TR=green BL=blue BR=yellow) for transform checks
    png(f"{tmp}/t.png", "-size", "32x20", "xc:red")  # placeholder, rebuilt below
    run(["convert", "-size", "32x20", "xc:red", "-size", "32x20", "xc:lime", "+append", f"{tmp}/top.png"])
    run(["convert", "-size", "32x20", "xc:blue", "-size", "32x20", "xc:yellow", "+append", f"{tmp}/bot.png"])
    run(["convert", f"{tmp}/top.png", f"{tmp}/bot.png", "-append", f"{tmp}/corners.png"])  # 64x40
    run(["convert", "-size", "32x32", "xc:red", "-size", "32x32", "xc:lime", "+append", f"{tmp}/ct.png"])
    run(["convert", "-size", "32x32", "xc:blue", "-size", "32x32", "xc:yellow", "+append", f"{tmp}/cb.png"])
    run(["convert", f"{tmp}/ct.png", f"{tmp}/cb.png", "-append", f"{tmp}/c64.png"])  # 64x64

    # heif-enc fixtures
    enc(f"{tmp}/rgb.png", f"{OUT}/single.heic")            # 64-coded clap->48 single
    enc(f"{tmp}/rgba.png", f"{OUT}/alpha.heic")            # auxiliary alpha plane
    enc(f"{tmp}/rgb.png", f"{OUT}/thumb.heic", "-t", "16")  # embedded thumbnail
    enc(f"{tmp}/gray.png", f"{OUT}/mono.heic")             # 4:0:0 monochrome
    enc(f"{tmp}/rgb16.png", f"{OUT}/depth10.heic", "-b", "10")  # 10-bit
    enc(f"{tmp}/plasma.png", f"{OUT}/grid.heic", "--cut-tiles", "48")  # 2x2 grid derived
    enc(f"{tmp}/rgb.png", f"{OUT}/bt709.heic", "--matrix_coefficients", "1",
        "--colour_primaries", "1", "--transfer_characteristic", "1")
    enc(f"{tmp}/rgb.png", f"{OUT}/bt2020pq.heic", "--matrix_coefficients", "9",
        "--colour_primaries", "9", "--transfer_characteristic", "16")
    enc(f"{tmp}/corners.png", f"{tmp}/corners.heic")       # non-square for transforms
    enc(f"{tmp}/c64.png", f"{tmp}/c64.heic", "-q", "92")    # CTU-aligned, no clap

    # property-injected transforms (corner-marked, dims/pixels observable)
    inject(f"{tmp}/corners.heic", f"{OUT}/irot90.heic", [irot(1)])
    inject(f"{tmp}/corners.heic", f"{OUT}/irot270.heic", [irot(3)])
    inject(f"{tmp}/corners.heic", f"{OUT}/imir_h.heic", [imir(1)])
    inject(f"{tmp}/corners.heic", f"{OUT}/imir_v.heic", [imir(0)])

    # derived items
    mux_derived(f"{OUT}/iden_rot90.heic", 64, 64, b"iden", 64, 64, f"{tmp}/c64.heic", irot_angle=1)
    mux_derived(f"{OUT}/iovl.heic", 64, 64, b"iovl", 96, 96, f"{tmp}/c64.heic", overlay_offset=(16, 16))

    # edge-case derived images: decode-path branches the happy fixtures miss.
    c64 = f"{tmp}/c64.heic"
    # negative offset -> src_skip clamping (spec-correct: shows tile's clipped
    # bottom-right region; libheif skips this — we are more compliant here)
    mux_overlay(f"{OUT}/iovl_negoff.heic", 96, 96, (-16, -16), src=c64)
    # flags&1 large format -> 32-bit canvas/offset path (== iovl pixel-for-pixel)
    mux_overlay(f"{OUT}/iovl_large.heic", 96, 96, (16, 16), large=True, src=c64)
    # version != 0 -> HeicError::Unsupported
    mux_overlay(f"{OUT}/iovl_badver.heic", 96, 96, (16, 16), version=1, src=c64)
    # oversized canvas -> Limits::check_dimensions rejection
    mux_overlay(f"{OUT}/iovl_huge_canvas.heic", 40000, 40000, (0, 0), large=True, src=c64)
    # iden chain depth 2 (ok) vs depth 4 (> MAX_DERIVED_DEPTH=3 -> reject)
    mux_iden_chain(f"{OUT}/iden_depth2.heic", 2, c64)
    mux_iden_chain(f"{OUT}/iden_depth4.heic", 4, c64)

    # metadata items (EXIF / XMP)
    shutil.copy(f"{OUT}/single.heic", f"{OUT}/exif.heic")
    run(["exiftool", "-overwrite_original", "-Make=ZenTest", "-Model=HeicFixture",
         "-DateTimeOriginal=2026:05:31 12:00:00", f"{OUT}/exif.heic"])
    shutil.copy(f"{OUT}/single.heic", f"{OUT}/xmp.heic")
    run(["exiftool", "-overwrite_original", "-XMP:Subject=zenfixture",
         "-XMP:Creator=heic-tests", f"{OUT}/xmp.heic"])

    # lossless (currently errors cleanly — cu_transquant_bypass unsupported; see CLAUDE.md)
    enc(f"{tmp}/rgb.png", f"{OUT}/lossless.heic", "-L")

    total = 0
    for f in sorted(os.listdir(OUT)):
        if f.endswith((".heic", ".heif")):
            sz = os.path.getsize(os.path.join(OUT, f)); total += sz
            print(f"  {sz:6d}B  {f}")
    print(f"  total {total} B ({total // 1024} KB) across {len(os.listdir(OUT))} files")


if __name__ == "__main__":
    main()
