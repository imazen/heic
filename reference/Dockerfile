# heif-ref: Reference HEIF decoder for comparison testing.
#
# Builds the latest libheif from source with libde265 (HEVC) and dav1d (AV1),
# then compiles heif_ref.c into a self-contained extraction tool.
#
# Build:
#   docker build -t ghcr.io/imazen/heif-ref:latest reference/
#
# Usage:
#   docker run --rm -v /path/to/files:/data ghcr.io/imazen/heif-ref \
#     extract /data/input.heic /data/output/
#
# The output directory will contain:
#   info.json      — all metadata (dimensions, color, EXIF/XMP/ICC presence, pixel stats)
#   primary.bin    — decoded RGB8 pixels (width*height*3 bytes)
#   thumbnail.bin  — thumbnail RGB8 (if present)
#   aux_<id>.bin   — auxiliary images as grayscale (gain map, depth, alpha, mattes)
#   exif.bin       — raw EXIF bytes (if present)
#   xmp.xml        — raw XMP (if present)
#   icc.bin        — raw ICC profile (if present)

# ── Stage 1: Build ─────────────────────────────────────────────────

FROM ubuntu:24.04 AS builder

ARG LIBHEIF_TAG=v1.19.7
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential cmake ninja-build pkg-config git ca-certificates \
    libde265-dev libdav1d-dev libaom-dev libx265-dev nasm \
    && rm -rf /var/lib/apt/lists/*

# Build libheif from source (latest release)
RUN git clone --depth 1 --branch ${LIBHEIF_TAG} \
        https://github.com/strukturag/libheif.git /build/libheif && \
    cmake -B /build/libheif/build -S /build/libheif -GNinja \
        -DCMAKE_INSTALL_PREFIX=/usr/local \
        -DCMAKE_BUILD_TYPE=Release \
        -DWITH_EXAMPLES=OFF \
        -DWITH_GDK_PIXBUF=OFF \
        -DBUILD_TESTING=OFF && \
    ninja -C /build/libheif/build && \
    ninja -C /build/libheif/build install && \
    ldconfig

# Build heif-ref tool
COPY heif_ref.c /build/
RUN gcc -O2 -Wall -Wextra -Wno-unused-parameter \
    -o /usr/local/bin/heif-ref /build/heif_ref.c \
    $(pkg-config --cflags --libs libheif) && \
    strip /usr/local/bin/heif-ref

# Verify it works
RUN /usr/local/bin/heif-ref version

# ── Stage 2: Runtime ───────────────────────────────────────────────

FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    libde265-0 libdav1d7 libaom3 libx265-199 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /usr/local/lib/libheif* /usr/local/lib/
COPY --from=builder /usr/local/bin/heif-ref /usr/local/bin/

RUN ldconfig

# Smoke test
RUN heif-ref version

ENTRYPOINT ["heif-ref"]
CMD ["version"]
