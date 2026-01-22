// Pure CABAC functions extracted from libde265 for comparison testing
// These are simplified/standalone versions without the full decoder context

#include <stdint.h>
#include <stdbool.h>

extern "C" {

// Simplified CABAC state for comparison
struct CabacState {
    uint32_t range;
    uint32_t value;
    int bits_needed;
    const uint8_t* bitstream_curr;
    const uint8_t* bitstream_end;
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

} // extern "C"
