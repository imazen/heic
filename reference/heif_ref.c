/*
 * heif-ref: Reference extraction tool for HEIC/HEIF testing.
 *
 * Links against libheif to extract all metadata and pixel data from a HEIF
 * file into a structured output directory. Used as a reference decoder to
 * compare against our pure-Rust heic crate.
 *
 * Output:
 *   <outdir>/info.json          — structured metadata (JSON)
 *   <outdir>/primary.bin        — decoded RGB8 pixels (width*height*3)
 *   <outdir>/thumbnail.bin      — thumbnail RGB8 (if present)
 *   <outdir>/aux_<id>.bin       — auxiliary image grayscale (if present)
 *   <outdir>/exif.bin           — raw EXIF bytes (if present)
 *   <outdir>/xmp.xml            — raw XMP bytes (if present)
 *   <outdir>/icc.bin            — raw ICC profile (if present)
 *
 * Build: gcc -O2 -Wall -o heif-ref heif_ref.c $(pkg-config --cflags --libs libheif)
 */

#include <libheif/heif.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <sys/stat.h>
#include <errno.h>

/* ── JSON writer ────────────────────────────────────────────────────── */

typedef struct {
    FILE *f;
    int depth;
    int first[32]; /* stack of "is first element" per nesting level */
} jw_t;

static void jw_init(jw_t *j, FILE *f) {
    memset(j, 0, sizeof(*j));
    j->f = f;
    j->first[0] = 1;
}

static void jw_indent(jw_t *j) {
    for (int i = 0; i < j->depth; i++) fprintf(j->f, "  ");
}

static void jw_sep(jw_t *j) {
    if (!j->first[j->depth]) fprintf(j->f, ",");
    fprintf(j->f, "\n");
    j->first[j->depth] = 0;
}

static void jw_obj_open(jw_t *j, const char *key) {
    jw_sep(j);
    jw_indent(j);
    if (key) fprintf(j->f, "\"%s\": {", key);
    else     fprintf(j->f, "{");
    j->depth++;
    j->first[j->depth] = 1;
}

static void jw_obj_close(jw_t *j) {
    fprintf(j->f, "\n");
    j->depth--;
    jw_indent(j);
    fprintf(j->f, "}");
}

static void jw_arr_open(jw_t *j, const char *key) {
    jw_sep(j);
    jw_indent(j);
    fprintf(j->f, "\"%s\": [", key);
    j->depth++;
    j->first[j->depth] = 1;
}

static void jw_arr_close(jw_t *j) {
    fprintf(j->f, "\n");
    j->depth--;
    jw_indent(j);
    fprintf(j->f, "]");
}

static void jw_key_int(jw_t *j, const char *key, long long val) {
    jw_sep(j);
    jw_indent(j);
    fprintf(j->f, "\"%s\": %lld", key, val);
}

static void jw_key_bool(jw_t *j, const char *key, int val) {
    jw_sep(j);
    jw_indent(j);
    fprintf(j->f, "\"%s\": %s", key, val ? "true" : "false");
}

/* Escape a string for JSON (handles \, ", control chars). */
static void jw_key_string(jw_t *j, const char *key, const char *val) {
    jw_sep(j);
    jw_indent(j);
    fprintf(j->f, "\"%s\": \"", key);
    if (val) {
        for (const char *p = val; *p; p++) {
            switch (*p) {
                case '"':  fprintf(j->f, "\\\""); break;
                case '\\': fprintf(j->f, "\\\\"); break;
                case '\n': fprintf(j->f, "\\n"); break;
                case '\r': fprintf(j->f, "\\r"); break;
                case '\t': fprintf(j->f, "\\t"); break;
                default:
                    if ((unsigned char)*p < 0x20)
                        fprintf(j->f, "\\u%04x", (unsigned char)*p);
                    else
                        fputc(*p, j->f);
            }
        }
    }
    fprintf(j->f, "\"");
}

static void jw_key_null(jw_t *j, const char *key) {
    jw_sep(j);
    jw_indent(j);
    fprintf(j->f, "\"%s\": null", key);
}

static void jw_key_double(jw_t *j, const char *key, double val) {
    jw_sep(j);
    jw_indent(j);
    fprintf(j->f, "\"%s\": %.6f", key, val);
}

/* ── Helpers ────────────────────────────────────────────────────────── */

static int write_bin(const char *dir, const char *name, const void *data, size_t len) {
    char path[4096];
    snprintf(path, sizeof(path), "%s/%s", dir, name);
    FILE *f = fopen(path, "wb");
    if (!f) { fprintf(stderr, "Cannot write %s: %s\n", path, strerror(errno)); return -1; }
    if (len > 0) fwrite(data, 1, len, f);
    fclose(f);
    return 0;
}

static int check_err(struct heif_error err, const char *context) {
    if (err.code != heif_error_Ok) {
        fprintf(stderr, "%s: %s (code=%d, subcode=%d)\n",
                context, err.message, err.code, err.subcode);
        return -1;
    }
    return 0;
}

/* Compute simple CRC32 (no dependency on zlib). */
static uint32_t crc32_buf(const uint8_t *data, size_t len) {
    uint32_t crc = 0xFFFFFFFF;
    for (size_t i = 0; i < len; i++) {
        crc ^= data[i];
        for (int b = 0; b < 8; b++)
            crc = (crc >> 1) ^ (0xEDB88320 & (-(crc & 1)));
    }
    return ~crc;
}

/* Compute per-channel stats for RGB8 data. */
static void rgb_stats(const uint8_t *data, size_t npixels,
                      int *r_min, int *r_max, double *r_mean,
                      int *g_min, int *g_max, double *g_mean,
                      int *b_min, int *b_max, double *b_mean) {
    *r_min = *g_min = *b_min = 255;
    *r_max = *g_max = *b_max = 0;
    uint64_t r_sum = 0, g_sum = 0, b_sum = 0;
    for (size_t i = 0; i < npixels; i++) {
        int r = data[i*3+0], g = data[i*3+1], b = data[i*3+2];
        if (r < *r_min) *r_min = r;
        if (r > *r_max) *r_max = r;
        r_sum += r;
        if (g < *g_min) *g_min = g;
        if (g > *g_max) *g_max = g;
        g_sum += g;
        if (b < *b_min) *b_min = b;
        if (b > *b_max) *b_max = b;
        b_sum += b;
    }
    *r_mean = (double)r_sum / (double)npixels;
    *g_mean = (double)g_sum / (double)npixels;
    *b_mean = (double)b_sum / (double)npixels;
}

/* Decode a heif_image_handle to packed RGB8 bytes. Returns malloc'd buffer.
 * Sets *out_stride to the row stride in bytes.  Caller frees. */
static uint8_t *decode_to_rgb(struct heif_image_handle *handle,
                              int width, int height, int *out_stride) {
    struct heif_image *img = NULL;
    struct heif_decoding_options *opts = heif_decoding_options_alloc();
    opts->ignore_transformations = 0;

    struct heif_error err = heif_decode_image(handle, &img,
                                              heif_colorspace_RGB,
                                              heif_chroma_interleaved_RGB,
                                              opts);
    heif_decoding_options_free(opts);
    if (err.code != heif_error_Ok) {
        fprintf(stderr, "decode_to_rgb: %s\n", err.message);
        return NULL;
    }

    int stride;
    const uint8_t *plane = heif_image_get_plane_readonly(img,
                                                          heif_channel_interleaved,
                                                          &stride);
    if (!plane) {
        heif_image_release(img);
        return NULL;
    }

    /* Re-read dimensions from decoded image (transforms may change them) */
    int dw = heif_image_get_width(img, heif_channel_interleaved);
    int dh = heif_image_get_height(img, heif_channel_interleaved);

    size_t packed_size = (size_t)dw * dh * 3;
    uint8_t *out = malloc(packed_size);
    if (!out) { heif_image_release(img); return NULL; }

    /* Copy row-by-row (stride may differ from width*3). */
    for (int y = 0; y < dh; y++)
        memcpy(out + y * dw * 3, plane + y * stride, dw * 3);

    *out_stride = dw * 3;
    heif_image_release(img);
    return out;
}

#if LIBHEIF_HAVE_VERSION(1,14,0)
/* Decode a handle to grayscale (mono Y plane). Returns malloc'd buffer. */
static uint8_t *decode_to_gray(struct heif_image_handle *handle,
                               int *out_w, int *out_h) {
    struct heif_image *img = NULL;
    struct heif_decoding_options *opts = heif_decoding_options_alloc();
    opts->ignore_transformations = 0;

    struct heif_error err = heif_decode_image(handle, &img,
                                              heif_colorspace_monochrome,
                                              heif_chroma_monochrome,
                                              opts);
    heif_decoding_options_free(opts);
    if (err.code != heif_error_Ok) {
        /* Fall back to RGB and take the R channel (some aux images may not
         * support monochrome decode). */
        err = heif_decode_image(handle, &img,
                                heif_colorspace_RGB,
                                heif_chroma_interleaved_RGB,
                                NULL);
        if (err.code != heif_error_Ok) {
            fprintf(stderr, "decode_to_gray fallback: %s\n", err.message);
            return NULL;
        }
        int stride;
        const uint8_t *plane = heif_image_get_plane_readonly(img,
                                                              heif_channel_interleaved,
                                                              &stride);
        int w = heif_image_get_width(img, heif_channel_interleaved);
        int h = heif_image_get_height(img, heif_channel_interleaved);
        size_t sz = (size_t)w * h;
        uint8_t *out = malloc(sz);
        if (out) {
            for (int y = 0; y < h; y++)
                for (int x = 0; x < w; x++)
                    out[y * w + x] = plane[y * stride + x * 3]; /* R channel */
        }
        *out_w = w;
        *out_h = h;
        heif_image_release(img);
        return out;
    }

    int stride;
    const uint8_t *plane = heif_image_get_plane_readonly(img, heif_channel_Y, &stride);
    if (!plane) { heif_image_release(img); return NULL; }

    int w = heif_image_get_width(img, heif_channel_Y);
    int h = heif_image_get_height(img, heif_channel_Y);
    size_t sz = (size_t)w * h;
    uint8_t *out = malloc(sz);
    if (out) {
        for (int y = 0; y < h; y++)
            memcpy(out + y * w, plane + y * stride, w);
    }
    *out_w = w;
    *out_h = h;
    heif_image_release(img);
    return out;
}
#endif /* LIBHEIF_HAVE_VERSION(1,14,0) */

/* ── Metadata extraction ───────────────────────────────────────────── */

static void extract_metadata_blocks(struct heif_image_handle *handle,
                                    const char *outdir, jw_t *j) {
    /* EXIF */
    int n_exif = heif_image_handle_get_number_of_metadata_blocks(handle, "Exif");
    if (n_exif > 0) {
        heif_item_id exif_id;
        heif_image_handle_get_list_of_metadata_block_IDs(handle, "Exif", &exif_id, 1);
        size_t exif_size = heif_image_handle_get_metadata_size(handle, exif_id);

        jw_obj_open(j, "exif");
        jw_key_bool(j, "present", 1);
        jw_key_int(j, "size", (long long)exif_size);

        if (exif_size > 0) {
            uint8_t *buf = malloc(exif_size);
            if (buf) {
                struct heif_error err = heif_image_handle_get_metadata(handle, exif_id, buf);
                if (err.code == heif_error_Ok) {
                    jw_key_int(j, "crc32", crc32_buf(buf, exif_size));
                    write_bin(outdir, "exif.bin", buf, exif_size);
                }
                free(buf);
            }
        }
        jw_obj_close(j);
    } else {
        jw_obj_open(j, "exif");
        jw_key_bool(j, "present", 0);
        jw_obj_close(j);
    }

    /* XMP (mime type with content_type application/rdf+xml) */
    int n_mime = heif_image_handle_get_number_of_metadata_blocks(handle, "mime");
    int xmp_found = 0;
    if (n_mime > 0) {
        heif_item_id *mime_ids = calloc(n_mime, sizeof(heif_item_id));
        heif_image_handle_get_list_of_metadata_block_IDs(handle, "mime", mime_ids, n_mime);
        for (int i = 0; i < n_mime; i++) {
            const char *ct = heif_image_handle_get_metadata_content_type(handle, mime_ids[i]);
            if (ct && strcmp(ct, "application/rdf+xml") == 0) {
                size_t xmp_size = heif_image_handle_get_metadata_size(handle, mime_ids[i]);
                jw_obj_open(j, "xmp");
                jw_key_bool(j, "present", 1);
                jw_key_int(j, "size", (long long)xmp_size);

                if (xmp_size > 0) {
                    uint8_t *buf = malloc(xmp_size);
                    if (buf) {
                        struct heif_error err = heif_image_handle_get_metadata(handle, mime_ids[i], buf);
                        if (err.code == heif_error_Ok) {
                            jw_key_int(j, "crc32", crc32_buf(buf, xmp_size));
                            write_bin(outdir, "xmp.xml", buf, xmp_size);
                        }
                        free(buf);
                    }
                }
                jw_obj_close(j);
                xmp_found = 1;
                break;
            }
        }
        free(mime_ids);
    }
    if (!xmp_found) {
        jw_obj_open(j, "xmp");
        jw_key_bool(j, "present", 0);
        jw_obj_close(j);
    }
}

static void extract_color_profile(struct heif_image_handle *handle,
                                  const char *outdir, jw_t *j) {
    enum heif_color_profile_type prof_type =
        heif_image_handle_get_color_profile_type(handle);

    jw_obj_open(j, "color_profile");

    if (prof_type == heif_color_profile_type_nclx) {
        jw_key_string(j, "type", "nclx");

        struct heif_color_profile_nclx *nclx = NULL;
        struct heif_error err = heif_image_handle_get_nclx_color_profile(handle, &nclx);
        if (err.code == heif_error_Ok && nclx) {
            jw_key_int(j, "color_primaries", nclx->color_primaries);
            jw_key_int(j, "transfer_characteristics", nclx->transfer_characteristics);
            jw_key_int(j, "matrix_coefficients", nclx->matrix_coefficients);
            jw_key_bool(j, "full_range", nclx->full_range_flag);
            heif_nclx_color_profile_free(nclx);
        }
    } else if (prof_type == heif_color_profile_type_rICC ||
               prof_type == heif_color_profile_type_prof) {
        jw_key_string(j, "type", prof_type == heif_color_profile_type_rICC ? "rICC" : "prof");

        size_t icc_size = heif_image_handle_get_raw_color_profile_size(handle);
        jw_key_int(j, "icc_size", (long long)icc_size);

        if (icc_size > 0) {
            uint8_t *buf = malloc(icc_size);
            if (buf) {
                struct heif_error err = heif_image_handle_get_raw_color_profile(handle, buf);
                if (err.code == heif_error_Ok) {
                    jw_key_int(j, "icc_crc32", crc32_buf(buf, icc_size));
                    write_bin(outdir, "icc.bin", buf, icc_size);
                }
                free(buf);
            }
        }
    } else {
        jw_key_string(j, "type", "none");
    }

    jw_obj_close(j);
}

/* ── Auxiliary images ──────────────────────────────────────────────── */

#if LIBHEIF_HAVE_VERSION(1,14,0)
static void extract_auxiliary_images(struct heif_image_handle *handle,
                                     const char *outdir, jw_t *j) {
    int n_aux = heif_image_handle_get_number_of_auxiliary_images(handle, 0);
    jw_arr_open(j, "auxiliary_images");

    if (n_aux > 0) {
        heif_item_id *aux_ids = calloc(n_aux, sizeof(heif_item_id));
        heif_image_handle_get_list_of_auxiliary_image_IDs(handle, 0, aux_ids, n_aux);

        for (int i = 0; i < n_aux; i++) {
            struct heif_image_handle *aux_handle = NULL;
            struct heif_error err = heif_image_handle_get_auxiliary_image_handle(
                handle, aux_ids[i], &aux_handle);
            if (err.code != heif_error_Ok) continue;

            const char *aux_type = NULL;
            heif_image_handle_get_auxiliary_type(aux_handle, &aux_type);

            int aux_w = heif_image_handle_get_width(aux_handle);
            int aux_h = heif_image_handle_get_height(aux_handle);
            int aux_bpp = heif_image_handle_get_luma_bits_per_pixel(aux_handle);

            jw_obj_open(j, NULL);
            jw_key_int(j, "id", aux_ids[i]);
            jw_key_string(j, "type", aux_type ? aux_type : "unknown");
            jw_key_int(j, "width", aux_w);
            jw_key_int(j, "height", aux_h);
            jw_key_int(j, "bit_depth", aux_bpp);

            /* Decode auxiliary image to grayscale */
            int dec_w = 0, dec_h = 0;
            uint8_t *gray = decode_to_gray(aux_handle, &dec_w, &dec_h);
            if (gray && dec_w > 0 && dec_h > 0) {
                size_t sz = (size_t)dec_w * dec_h;
                jw_key_int(j, "decoded_width", dec_w);
                jw_key_int(j, "decoded_height", dec_h);
                jw_key_int(j, "pixel_crc32", crc32_buf(gray, sz));

                char fname[256];
                snprintf(fname, sizeof(fname), "aux_%u.bin", aux_ids[i]);
                write_bin(outdir, fname, gray, sz);
                free(gray);
            }

            /* Check for XMP on this auxiliary item (gain map metadata) */
            int aux_n_mime = heif_image_handle_get_number_of_metadata_blocks(aux_handle, "mime");
            if (aux_n_mime > 0) {
                heif_item_id *ids = calloc(aux_n_mime, sizeof(heif_item_id));
                heif_image_handle_get_list_of_metadata_block_IDs(aux_handle, "mime", ids, aux_n_mime);
                for (int mi = 0; mi < aux_n_mime; mi++) {
                    const char *ct = heif_image_handle_get_metadata_content_type(aux_handle, ids[mi]);
                    if (ct && strcmp(ct, "application/rdf+xml") == 0) {
                        size_t xmp_sz = heif_image_handle_get_metadata_size(aux_handle, ids[mi]);
                        jw_key_int(j, "xmp_size", (long long)xmp_sz);
                        if (xmp_sz > 0) {
                            uint8_t *xbuf = malloc(xmp_sz);
                            if (xbuf) {
                                struct heif_error xerr = heif_image_handle_get_metadata(aux_handle, ids[mi], xbuf);
                                if (xerr.code == heif_error_Ok) {
                                    char xname[256];
                                    snprintf(xname, sizeof(xname), "aux_%u_xmp.xml", aux_ids[i]);
                                    write_bin(outdir, xname, xbuf, xmp_sz);
                                }
                                free(xbuf);
                            }
                        }
                        break;
                    }
                }
                free(ids);
            }

            if (aux_type) {
#if LIBHEIF_HAVE_VERSION(1,18,0)
                /* API changed: takes handle + pointer-to-type since 1.18 */
                heif_image_handle_free_auxiliary_types(aux_handle, &aux_type);
#else
                heif_image_handle_free_auxiliary_types(&aux_type);
#endif
            }
            heif_image_handle_release(aux_handle);
            jw_obj_close(j);
        }
        free(aux_ids);
    }

    jw_arr_close(j);
}
#endif /* LIBHEIF_HAVE_VERSION(1,14,0) */

/* ── Thumbnail ─────────────────────────────────────────────────────── */

static void extract_thumbnail(struct heif_image_handle *handle,
                              const char *outdir, jw_t *j) {
    int n_thumb = heif_image_handle_get_number_of_thumbnails(handle);
    jw_obj_open(j, "thumbnail");

    if (n_thumb > 0) {
        heif_item_id thumb_id;
        heif_image_handle_get_list_of_thumbnail_IDs(handle, &thumb_id, 1);

        struct heif_image_handle *thumb_handle = NULL;
        struct heif_error err = heif_image_handle_get_thumbnail(handle, thumb_id, &thumb_handle);
        if (err.code == heif_error_Ok && thumb_handle) {
            int tw = heif_image_handle_get_width(thumb_handle);
            int th = heif_image_handle_get_height(thumb_handle);
            int tbpp = heif_image_handle_get_luma_bits_per_pixel(thumb_handle);

            jw_key_bool(j, "present", 1);
            jw_key_int(j, "width", tw);
            jw_key_int(j, "height", th);
            jw_key_int(j, "bit_depth", tbpp);

            int stride;
            uint8_t *rgb = decode_to_rgb(thumb_handle, tw, th, &stride);
            if (rgb) {
                /* Re-read decoded dims (transforms may change them) */
                size_t sz = (size_t)tw * th * 3;
                jw_key_int(j, "pixel_crc32", crc32_buf(rgb, sz));
                write_bin(outdir, "thumbnail.bin", rgb, sz);
                free(rgb);
            }

            heif_image_handle_release(thumb_handle);
        } else {
            jw_key_bool(j, "present", 0);
        }
    } else {
        jw_key_bool(j, "present", 0);
    }

    jw_obj_close(j);
}

/* ── Primary image ─────────────────────────────────────────────────── */

static int extract_primary(struct heif_context *ctx, const char *outdir, jw_t *j) {
    struct heif_image_handle *handle = NULL;
    struct heif_error err = heif_context_get_primary_image_handle(ctx, &handle);
    if (check_err(err, "get_primary_image_handle")) return -1;

    int width = heif_image_handle_get_width(handle);
    int height = heif_image_handle_get_height(handle);
    int has_alpha = heif_image_handle_has_alpha_channel(handle);
    int bit_depth = heif_image_handle_get_luma_bits_per_pixel(handle);
    int chroma_bpp = heif_image_handle_get_chroma_bits_per_pixel(handle);
    int ispe_w = heif_image_handle_get_ispe_width(handle);
    int ispe_h = heif_image_handle_get_ispe_height(handle);

    jw_obj_open(j, "primary");
    jw_key_int(j, "width", width);
    jw_key_int(j, "height", height);
    jw_key_int(j, "ispe_width", ispe_w);
    jw_key_int(j, "ispe_height", ispe_h);
    jw_key_bool(j, "has_alpha", has_alpha);
    jw_key_int(j, "bit_depth", bit_depth);
    jw_key_int(j, "chroma_bits_per_pixel", chroma_bpp);

    /* Color profile */
    extract_color_profile(handle, outdir, j);

    /* Decode to RGB8 */
    int stride;
    uint8_t *rgb = decode_to_rgb(handle, width, height, &stride);
    if (rgb) {
        size_t npixels = (size_t)width * height;
        size_t sz = npixels * 3;

        int r_min, r_max, g_min, g_max, b_min, b_max;
        double r_mean, g_mean, b_mean;
        rgb_stats(rgb, npixels, &r_min, &r_max, &r_mean,
                  &g_min, &g_max, &g_mean, &b_min, &b_max, &b_mean);

        jw_key_int(j, "pixel_crc32", crc32_buf(rgb, sz));

        jw_obj_open(j, "pixel_stats");
        jw_key_int(j, "r_min", r_min); jw_key_int(j, "r_max", r_max);
        jw_key_double(j, "r_mean", r_mean);
        jw_key_int(j, "g_min", g_min); jw_key_int(j, "g_max", g_max);
        jw_key_double(j, "g_mean", g_mean);
        jw_key_int(j, "b_min", b_min); jw_key_int(j, "b_max", b_max);
        jw_key_double(j, "b_mean", b_mean);
        jw_obj_close(j); /* pixel_stats */

        write_bin(outdir, "primary.bin", rgb, sz);
        free(rgb);
    } else {
        jw_key_null(j, "pixel_crc32");
    }

    /* Metadata blocks (EXIF, XMP) */
    extract_metadata_blocks(handle, outdir, j);

    /* Thumbnail */
    extract_thumbnail(handle, outdir, j);

    /* Auxiliary images */
#if LIBHEIF_HAVE_VERSION(1,14,0)
    extract_auxiliary_images(handle, outdir, j);
#endif

    jw_obj_close(j); /* primary */

    heif_image_handle_release(handle);
    return 0;
}

/* ── Top-level image enumeration ───────────────────────────────────── */

static void enumerate_top_level(struct heif_context *ctx, jw_t *j) {
    int n = heif_context_get_number_of_top_level_images(ctx);
    jw_key_int(j, "num_top_level_images", n);

    if (n > 0 && n <= 1024) {
        heif_item_id *ids = calloc(n, sizeof(heif_item_id));
        heif_context_get_list_of_top_level_image_IDs(ctx, ids, n);

        jw_arr_open(j, "top_level_images");
        for (int i = 0; i < n; i++) {
            struct heif_image_handle *h = NULL;
            struct heif_error err = heif_context_get_image_handle(ctx, ids[i], &h);
            if (err.code != heif_error_Ok) continue;

            jw_obj_open(j, NULL);
            jw_key_int(j, "id", ids[i]);
            jw_key_int(j, "width", heif_image_handle_get_width(h));
            jw_key_int(j, "height", heif_image_handle_get_height(h));
            jw_key_bool(j, "has_alpha", heif_image_handle_has_alpha_channel(h));
            jw_key_int(j, "bit_depth", heif_image_handle_get_luma_bits_per_pixel(h));
            jw_key_bool(j, "is_primary", heif_image_handle_is_primary_image(h));
            jw_obj_close(j);

            heif_image_handle_release(h);
        }
        jw_arr_close(j);
        free(ids);
    }
}

/* ── Commands ──────────────────────────────────────────────────────── */

static int cmd_extract(const char *input_path, const char *outdir) {
    /* Create output directory */
    mkdir(outdir, 0755);

    /* Read input file */
    FILE *fin = fopen(input_path, "rb");
    if (!fin) { fprintf(stderr, "Cannot open %s: %s\n", input_path, strerror(errno)); return 1; }
    fseek(fin, 0, SEEK_END);
    long fsize = ftell(fin);
    fseek(fin, 0, SEEK_SET);
    uint8_t *data = malloc(fsize);
    if (!data) { fclose(fin); return 1; }
    if ((long)fread(data, 1, fsize, fin) != fsize) {
        fprintf(stderr, "Short read on %s\n", input_path);
        free(data);
        fclose(fin);
        return 1;
    }
    fclose(fin);

    /* Create heif context */
    struct heif_context *ctx = heif_context_alloc();
    struct heif_error err = heif_context_read_from_memory_without_copy(ctx, data, fsize, NULL);
    if (check_err(err, "read_from_memory")) {
        heif_context_free(ctx);
        free(data);
        return 1;
    }

    /* Open JSON output */
    char json_path[4096];
    snprintf(json_path, sizeof(json_path), "%s/info.json", outdir);
    FILE *jf = fopen(json_path, "w");
    if (!jf) {
        fprintf(stderr, "Cannot create %s\n", json_path);
        heif_context_free(ctx);
        free(data);
        return 1;
    }

    jw_t j;
    jw_init(&j, jf);
    jw_obj_open(&j, NULL);

    jw_key_int(&j, "schema_version", 1);
    jw_key_string(&j, "libheif_version", heif_get_version());
    jw_key_string(&j, "input_file", input_path);
    jw_key_int(&j, "file_size", fsize);

    /* Enumerate top-level images */
    enumerate_top_level(ctx, &j);

    /* Extract primary image and all associated data */
    extract_primary(ctx, outdir, &j);

    jw_obj_close(&j);
    fprintf(jf, "\n");
    fclose(jf);

    heif_context_free(ctx);
    free(data);

    fprintf(stderr, "Extracted to %s\n", outdir);
    return 0;
}

static int cmd_version(void) {
    printf("heif-ref 1.0\n");
    printf("libheif %s\n", heif_get_version());
    printf("libheif numeric: %d\n", heif_get_version_number());
    return 0;
}

static void usage(const char *argv0) {
    fprintf(stderr, "Usage:\n");
    fprintf(stderr, "  %s extract <input.heic> <output_dir>\n", argv0);
    fprintf(stderr, "  %s version\n", argv0);
    fprintf(stderr, "\n");
    fprintf(stderr, "Extract all metadata and decoded pixels from a HEIF file\n");
    fprintf(stderr, "using libheif as reference, for comparison testing.\n");
}

int main(int argc, char **argv) {
    if (argc < 2) { usage(argv[0]); return 1; }

    if (strcmp(argv[1], "extract") == 0) {
        if (argc < 4) { usage(argv[0]); return 1; }
        return cmd_extract(argv[2], argv[3]);
    } else if (strcmp(argv[1], "version") == 0) {
        return cmd_version();
    } else {
        fprintf(stderr, "Unknown command: %s\n", argv[1]);
        usage(argv[0]);
        return 1;
    }
}
