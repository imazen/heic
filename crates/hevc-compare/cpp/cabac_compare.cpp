// Pure CABAC functions extracted from libde265 for comparison testing
// These are simplified/standalone versions without the full decoder context

#include <stdint.h>
#include <stdbool.h>

extern "C" {

// Context model (same as libde265)
struct ContextModel {
    uint8_t state;
    uint8_t mps;
};

// Simplified CABAC state for comparison
struct CabacState {
    uint32_t range;
    uint32_t value;
    int bits_needed;
    const uint8_t* bitstream_curr;
    const uint8_t* bitstream_end;
};

// CABAC tables from H.265 spec (same as libde265)
static const uint8_t LPS_table[64][4] = {
    { 128, 176, 208, 240}, { 128, 167, 197, 227}, { 128, 158, 187, 216}, { 123, 150, 178, 205},
    { 116, 142, 169, 195}, { 111, 135, 160, 185}, { 105, 128, 152, 175}, { 100, 122, 144, 166},
    {  95, 116, 137, 158}, {  90, 110, 130, 150}, {  85, 104, 123, 142}, {  81,  99, 117, 135},
    {  77,  94, 111, 128}, {  73,  89, 105, 122}, {  69,  85, 100, 116}, {  66,  80,  95, 110},
    {  62,  76,  90, 104}, {  59,  72,  86,  99}, {  56,  69,  81,  94}, {  53,  65,  77,  89},
    {  51,  62,  73,  85}, {  48,  59,  69,  80}, {  46,  56,  66,  76}, {  43,  53,  63,  72},
    {  41,  50,  59,  69}, {  39,  48,  56,  65}, {  37,  45,  54,  62}, {  35,  43,  51,  59},
    {  33,  41,  48,  56}, {  32,  39,  46,  53}, {  30,  37,  43,  50}, {  29,  35,  41,  48},
    {  27,  33,  39,  45}, {  26,  31,  37,  43}, {  24,  30,  35,  41}, {  23,  28,  33,  39},
    {  22,  27,  32,  37}, {  21,  26,  30,  35}, {  20,  24,  29,  33}, {  19,  23,  27,  31},
    {  18,  22,  26,  30}, {  17,  21,  25,  28}, {  16,  20,  23,  27}, {  15,  19,  22,  25},
    {  14,  18,  21,  24}, {  14,  17,  20,  23}, {  13,  16,  19,  22}, {  12,  15,  18,  21},
    {  12,  14,  17,  20}, {  11,  14,  16,  19}, {  11,  13,  15,  18}, {  10,  12,  15,  17},
    {  10,  12,  14,  16}, {   9,  11,  13,  15}, {   9,  11,  12,  14}, {   8,  10,  12,  14},
    {   8,   9,  11,  13}, {   7,   9,  11,  12}, {   7,   9,  10,  12}, {   7,   8,  10,  11},
    {   6,   8,   9,  11}, {   6,   7,   9,  10}, {   6,   7,   8,   9}, {   2,   2,   2,   2}
};

static const uint8_t renorm_table[32] = {
    6,  5,  4,  4,  3,  3,  3,  3,  2,  2,  2,  2,  2,  2,  2,  2,
    1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1,  1
};

static const uint8_t next_state_MPS[64] = {
    1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,
    17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,
    33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,
    49,50,51,52,53,54,55,56,57,58,59,60,61,62,62,63
};

static const uint8_t next_state_LPS[64] = {
    0,0,1,2,2,4,4,5,6,7,8,9,9,11,11,12,
    13,13,15,15,16,16,18,18,19,19,21,21,22,22,23,24,
    24,25,26,26,27,27,28,29,29,30,30,30,31,32,32,33,
    33,33,34,34,35,35,35,36,36,36,37,37,37,38,38,63
};

// Initialize CABAC state
void cabac_init(CabacState* state, const uint8_t* data, int length) {
    state->range = 510;
    state->bits_needed = 8;
    state->bitstream_curr = data;
    state->bitstream_end = data + length;

    // Read initial value (9 bits)
    state->value = 0;
    state->bits_needed = -8;
    if (state->bitstream_curr < state->bitstream_end) {
        state->value = *state->bitstream_curr++;
    }
    state->value <<= 8;
    state->bits_needed = 0;
    if (state->bitstream_curr < state->bitstream_end) {
        state->value |= *state->bitstream_curr++;
        state->bits_needed = -8;
    }
}

// Bypass decode - matches libde265 exactly
int cabac_decode_bypass(CabacState* state) {
    state->value <<= 1;
    state->bits_needed++;

    if (state->bits_needed >= 0) {
        if (state->bitstream_end > state->bitstream_curr) {
            state->bits_needed = -8;
            state->value |= *state->bitstream_curr++;
        } else {
            state->bits_needed = -8;
        }
    }

    int bit;
    uint32_t scaled_range = state->range << 7;
    if (state->value >= scaled_range) {
        state->value -= scaled_range;
        bit = 1;
    } else {
        bit = 0;
    }

    return bit;
}

// Decode fixed-length bypass bits
uint32_t cabac_decode_bypass_bits(CabacState* state, int num_bits) {
    uint32_t value = 0;
    for (int i = 0; i < num_bits; i++) {
        value = (value << 1) | cabac_decode_bypass(state);
    }
    return value;
}

// Decode coeff_abs_level_remaining - matches libde265 exactly
int cabac_decode_coeff_abs_level_remaining(CabacState* state, int rice_param) {
    // Count prefix (unary 1s terminated by 0)
    int prefix = 0;
    while (cabac_decode_bypass(state) != 0 && prefix < 32) {
        prefix++;
    }

    int value;
    if (prefix <= 3) {
        // TR part only
        uint32_t suffix = cabac_decode_bypass_bits(state, rice_param);
        value = (prefix << rice_param) + suffix;
    } else {
        // EGk part
        int suffix_bits = prefix - 3 + rice_param;
        uint32_t suffix = cabac_decode_bypass_bits(state, suffix_bits);
        value = (((1 << (prefix - 3)) + 3 - 1) << rice_param) + suffix;
    }

    return value;
}

// Get current state for comparison
void cabac_get_state(const CabacState* state, uint32_t* range, uint32_t* value, int* bits_needed) {
    *range = state->range;
    *value = state->value;
    *bits_needed = state->bits_needed;
}

// Initialize context model for a given init_value and slice_qp
void context_init(ContextModel* ctx, uint8_t init_value, int slice_qp) {
    int slope = (init_value >> 4) * 5 - 45;
    int offset = ((init_value & 15) << 3) - 16;

    int init_state = ((slope * (slice_qp - 16)) >> 4) + offset;
    if (init_state < 1) init_state = 1;
    if (init_state > 126) init_state = 126;

    if (init_state >= 64) {
        ctx->state = init_state - 64;
        ctx->mps = 1;
    } else {
        ctx->state = 63 - init_state;
        ctx->mps = 0;
    }
}

// Get context state for comparison
void context_get_state(const ContextModel* ctx, uint8_t* state, uint8_t* mps) {
    *state = ctx->state;
    *mps = ctx->mps;
}

// Decode a context-coded bin - matches libde265 exactly
int cabac_decode_bin(CabacState* decoder, ContextModel* model) {
    int decoded_bit;
    int LPS = LPS_table[model->state][(decoder->range >> 6) - 4];
    decoder->range -= LPS;

    uint32_t scaled_range = decoder->range << 7;

    if (decoder->value < scaled_range) {
        // MPS path
        decoded_bit = model->mps;
        model->state = next_state_MPS[model->state];

        if (scaled_range < (256 << 7)) {
            // Renormalize: shift range by one bit
            decoder->range = scaled_range >> 6;
            decoder->value <<= 1;
            decoder->bits_needed++;

            if (decoder->bits_needed == 0) {
                decoder->bits_needed = -8;
                if (decoder->bitstream_curr < decoder->bitstream_end) {
                    decoder->value |= *decoder->bitstream_curr++;
                }
            }
        }
    } else {
        // LPS path
        decoder->value = decoder->value - scaled_range;

        int num_bits = renorm_table[LPS >> 3];
        decoder->value <<= num_bits;
        decoder->range = LPS << num_bits;

        decoded_bit = 1 - model->mps;

        if (model->state == 0) {
            model->mps = 1 - model->mps;
        }
        model->state = next_state_LPS[model->state];

        decoder->bits_needed += num_bits;

        if (decoder->bits_needed >= 0) {
            if (decoder->bitstream_curr < decoder->bitstream_end) {
                decoder->value |= (*decoder->bitstream_curr++) << decoder->bits_needed;
            }
            decoder->bits_needed -= 8;
        }
    }

    return decoded_bit;
}

// Decode last_significant_coeff_prefix - first stage of coefficient decode
// Returns the prefix value (0 to cMax)
int decode_last_significant_coeff_prefix(
    CabacState* decoder,
    ContextModel* contexts,  // Array of context models
    int log2_size,
    int c_idx
) {
    int cMax = (log2_size << 1) - 1;

    int ctxOffset, ctxShift;
    if (c_idx == 0) {
        ctxOffset = 3 * (log2_size - 2) + ((log2_size - 1) >> 2);
        ctxShift = (log2_size + 1) >> 2;
    } else {
        ctxOffset = 15;
        ctxShift = log2_size - 2;
    }

    int value = cMax;
    for (int binIdx = 0; binIdx < cMax; binIdx++) {
        int ctxIdxInc = binIdx >> ctxShift;
        int bit = cabac_decode_bin(decoder, &contexts[ctxOffset + ctxIdxInc]);
        if (bit == 0) {
            value = binIdx;
            break;
        }
    }

    return value;
}

// Decode last_significant_coeff suffix (if prefix > 3)
int decode_last_significant_coeff_suffix(CabacState* decoder, int prefix) {
    if (prefix > 3) {
        int nBits = (prefix >> 1) - 1;
        int suffix = cabac_decode_bypass_bits(decoder, nBits);
        return ((2 + (prefix & 1)) << nBits) + suffix;
    } else {
        return prefix;
    }
}

// Full last_significant_coeff decode (x or y)
// contexts should point to LAST_SIGNIFICANT_COEFFICIENT_X_PREFIX or Y_PREFIX
int decode_last_significant_coeff(
    CabacState* decoder,
    ContextModel* contexts,
    int log2_size,
    int c_idx
) {
    int prefix = decode_last_significant_coeff_prefix(decoder, contexts, log2_size, c_idx);
    return decode_last_significant_coeff_suffix(decoder, prefix);
}

// Structure to hold result of last_sig decode for comparison
struct LastSigResult {
    int x;
    int y;
    uint32_t cabac_range;
    uint32_t cabac_value;
    int cabac_bits_needed;
};

// Decode both last_x and last_y and return results for comparison
void decode_last_significant_coeff_xy(
    CabacState* decoder,
    ContextModel* ctx_x,  // LAST_SIGNIFICANT_COEFFICIENT_X_PREFIX contexts
    ContextModel* ctx_y,  // LAST_SIGNIFICANT_COEFFICIENT_Y_PREFIX contexts
    int log2_size,
    int c_idx,
    int scan_idx,  // 0=diag, 1=horiz, 2=vert
    LastSigResult* result
) {
    int last_x = decode_last_significant_coeff(decoder, ctx_x, log2_size, c_idx);
    int last_y = decode_last_significant_coeff(decoder, ctx_y, log2_size, c_idx);

    // Swap for vertical scan
    if (scan_idx == 2) {
        int tmp = last_x;
        last_x = last_y;
        last_y = tmp;
    }

    result->x = last_x;
    result->y = last_y;
    result->cabac_range = decoder->range;
    result->cabac_value = decoder->value;
    result->cabac_bits_needed = decoder->bits_needed;
}

// Decode coded_sub_block_flag
int decode_coded_sub_block_flag(
    CabacState* decoder,
    ContextModel* contexts,  // CODED_SUB_BLOCK_FLAG contexts (4 total)
    int c_idx,
    int csbf_neighbors  // bit0=right, bit1=below
) {
    // csbfCtx = 1 if either neighbor is coded, else 0
    int csbfCtx = ((csbf_neighbors & 1) | (csbf_neighbors >> 1)) ? 1 : 0;
    int ctxIdx = csbfCtx + (c_idx != 0 ? 2 : 0);
    return cabac_decode_bin(decoder, &contexts[ctxIdx]);
}

// Decode sig_coeff_flag with full context derivation
int decode_sig_coeff_flag(
    CabacState* decoder,
    ContextModel* contexts,  // SIG_COEFF_FLAG contexts (44 for luma, 16 for chroma)
    int x_c,       // x position in TU
    int y_c,       // y position in TU
    int log2_size, // log2 of TU size
    int c_idx,     // 0=luma, 1/2=chroma
    int scan_idx,  // 0=diag, 1=horiz, 2=vert
    int prev_csbf  // neighbor coded sub-block flags: bit0=right, bit1=below
) {
    int sb_width = 1 << (log2_size - 2);
    int sigCtx;

    // 4x4 TU special case
    if (sb_width == 1) {
        static const uint8_t ctxIdxMap[16] = {
            0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8
        };
        sigCtx = ctxIdxMap[(y_c << 2) + x_c];
    }
    else if (x_c == 0 && y_c == 0) {
        sigCtx = 0;
    }
    else {
        int x_s = x_c >> 2;
        int y_s = y_c >> 2;
        int x_p = x_c & 3;
        int y_p = y_c & 3;

        switch (prev_csbf) {
            case 0:
                sigCtx = (x_p + y_p >= 3) ? 0 : (x_p + y_p > 0) ? 1 : 2;
                break;
            case 1:  // Right neighbor coded (bit0=1)
                sigCtx = (y_p == 0) ? 2 : (y_p == 1) ? 1 : 0;
                break;
            case 2:  // Below neighbor coded (bit1=1)
                sigCtx = (x_p == 0) ? 2 : (x_p == 1) ? 1 : 0;
                break;
            default:  // Both neighbors coded
                sigCtx = 2;
                break;
        }

        if (c_idx == 0) {
            if (x_s + y_s > 0) sigCtx += 3;

            if (sb_width == 2) {  // 8x8 TU
                sigCtx += (scan_idx == 0) ? 9 : 15;
            } else {
                sigCtx += 21;
            }
        } else {
            if (sb_width == 2) {
                sigCtx += 9;
            } else {
                sigCtx += 12;
            }
        }
    }

    int ctxIdxInc = (c_idx == 0) ? sigCtx : (27 + sigCtx);
    return cabac_decode_bin(decoder, &contexts[ctxIdxInc]);
}

// Decode coeff_abs_level_greater1_flag
// Per H.265 section 9.3.4.2.7
int decode_coeff_abs_level_greater1_flag(
    CabacState* decoder,
    ContextModel* contexts,  // COEFF_ABS_LEVEL_GREATER1_FLAG contexts (24 total: 16 luma + 8 chroma)
    int c_idx,              // 0=luma, 1/2=chroma
    int ctx_set,            // 0-3, based on sub-block position and previous sub-block c1
    int greater1_ctx        // 0-3, state machine for this sub-block
) {
    // ctxIdx = ctxSet*4 + min(greater1Ctx, 3) + (c_idx > 0 ? 16 : 0)
    int ctx_inc = ctx_set * 4 + (greater1_ctx < 4 ? greater1_ctx : 3);
    if (c_idx > 0) ctx_inc += 16;
    return cabac_decode_bin(decoder, &contexts[ctx_inc]);
}

// Decode coeff_abs_level_greater2_flag
// Per H.265 section 9.3.4.2.8
int decode_coeff_abs_level_greater2_flag(
    CabacState* decoder,
    ContextModel* contexts,  // COEFF_ABS_LEVEL_GREATER2_FLAG contexts (6 total: 4 luma + 2 chroma)
    int c_idx,              // 0=luma, 1/2=chroma
    int ctx_set             // 0-3 for luma, 0-1 for chroma
) {
    // ctxIdx = ctxSet + (c_idx > 0 ? 4 : 0)
    int ctx_inc = ctx_set + (c_idx > 0 ? 4 : 0);
    return cabac_decode_bin(decoder, &contexts[ctx_inc]);
}

// Calculate ctxSet for greater1_flag/greater2_flag
// Returns: ctxSet (0-3)
// Per H.265:
//   - For luma non-DC subblock: base = 2
//   - For DC subblock or chroma: base = 0
//   - If previous subblock ended with c1 == 0: ctxSet = base + 1, else ctxSet = base
int calc_ctx_set(
    int sb_idx,      // sub-block index (0 = DC sub-block)
    int c_idx,       // 0=luma, 1/2=chroma
    int prev_gt1     // 1 if previous sub-block had any greater1_flag == 1
) {
    int base;
    if (sb_idx == 0 || c_idx != 0) {
        base = 0;
    } else {
        base = 2;
    }

    // prev_gt1 indicates whether the previous sub-block's c1 ended at 0
    // c1 == 0 when any coefficient in prev subblock had greater1_flag == 1
    return base + (prev_gt1 ? 1 : 0);
}

} // extern "C"
