//! HEVC function comparison crate
//!
//! Compares pure functions between libde265 (C++) and our Rust implementation
//! to find divergence points.

#![allow(non_camel_case_types)]

use std::ffi::c_int;

// FFI bindings to C++ functions
#[repr(C)]
pub struct CabacState {
    range: u32,
    value: u32,
    bits_needed: c_int,
    bitstream_curr: *const u8,
    bitstream_end: *const u8,
}

unsafe extern "C" {
    fn cabac_init(state: *mut CabacState, data: *const u8, length: c_int);
    fn cabac_decode_bypass(state: *mut CabacState) -> c_int;
    fn cabac_decode_bypass_bits(state: *mut CabacState, num_bits: c_int) -> u32;
    fn cabac_decode_coeff_abs_level_remaining(state: *mut CabacState, rice_param: c_int) -> c_int;
    fn cabac_get_state(state: *const CabacState, range: *mut u32, value: *mut u32, bits_needed: *mut c_int);
}

/// C++ CABAC decoder wrapper
pub struct CppCabac {
    state: CabacState,
    // Keep the data alive - leaked to ensure stable address
    _data: &'static [u8],
}

impl CppCabac {
    pub fn new(data: &[u8]) -> Self {
        // Leak the data to get a stable address (for testing only)
        let data_leaked: &'static [u8] = Box::leak(data.to_vec().into_boxed_slice());

        let mut state = CabacState {
            range: 0,
            value: 0,
            bits_needed: 0,
            bitstream_curr: std::ptr::null(),
            bitstream_end: std::ptr::null(),
        };

        unsafe {
            cabac_init(&mut state, data_leaked.as_ptr(), data_leaked.len() as c_int);
        }

        Self {
            state,
            _data: data_leaked,
        }
    }

    pub fn decode_bypass(&mut self) -> u32 {
        unsafe { cabac_decode_bypass(&mut self.state) as u32 }
    }

    pub fn decode_bypass_bits(&mut self, num_bits: u8) -> u32 {
        unsafe { cabac_decode_bypass_bits(&mut self.state, num_bits as c_int) }
    }

    pub fn decode_coeff_abs_level_remaining(&mut self, rice_param: u8) -> i32 {
        unsafe { cabac_decode_coeff_abs_level_remaining(&mut self.state, rice_param as c_int) }
    }

    pub fn get_state(&self) -> (u32, u32, i32) {
        let mut range = 0u32;
        let mut value = 0u32;
        let mut bits_needed = 0i32;
        unsafe {
            cabac_get_state(&self.state, &mut range, &mut value, &mut bits_needed);
        }
        (range, value, bits_needed)
    }
}

/// Rust CABAC decoder (our implementation)
pub struct RustCabac<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    value: u32,
    bits_needed: i32,
}

impl<'a> RustCabac<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut cabac = Self {
            data,
            pos: 0,
            range: 510,
            value: 0,
            bits_needed: 8,
        };

        // Initialize value (matching C++ init)
        cabac.bits_needed = -8;
        if cabac.pos < cabac.data.len() {
            cabac.value = cabac.data[cabac.pos] as u32;
            cabac.pos += 1;
        }
        cabac.value <<= 8;
        cabac.bits_needed = 0;
        if cabac.pos < cabac.data.len() {
            cabac.value |= cabac.data[cabac.pos] as u32;
            cabac.pos += 1;
            cabac.bits_needed = -8;
        }

        cabac
    }

    pub fn decode_bypass(&mut self) -> u32 {
        self.value <<= 1;
        self.bits_needed += 1;

        if self.bits_needed >= 0 {
            if self.pos < self.data.len() {
                self.bits_needed = -8;
                self.value |= self.data[self.pos] as u32;
                self.pos += 1;
            } else {
                self.bits_needed = -8;
            }
        }

        let scaled_range = self.range << 7;
        if self.value >= scaled_range {
            self.value -= scaled_range;
            1
        } else {
            0
        }
    }

    pub fn decode_bypass_bits(&mut self, num_bits: u8) -> u32 {
        let mut value = 0u32;
        for _ in 0..num_bits {
            value = (value << 1) | self.decode_bypass();
        }
        value
    }

    pub fn decode_coeff_abs_level_remaining(&mut self, rice_param: u8) -> i32 {
        // Count prefix (unary 1s terminated by 0)
        let mut prefix = 0u32;
        while self.decode_bypass() != 0 && prefix < 32 {
            prefix += 1;
        }

        let value = if prefix <= 3 {
            // TR part only
            let suffix = self.decode_bypass_bits(rice_param);
            ((prefix << rice_param) + suffix) as i32
        } else {
            // EGk part
            let suffix_bits = (prefix - 3 + rice_param as u32) as u8;
            let suffix = self.decode_bypass_bits(suffix_bits);
            let base = ((1u32 << (prefix - 3)) + 2) << rice_param;
            (base + suffix) as i32
        };

        value
    }

    pub fn get_state(&self) -> (u32, u32, i32) {
        (self.range, self.value, self.bits_needed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test data - random bytes for CABAC testing
    const TEST_DATA: &[u8] = &[
        0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0,
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
        0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x01,
        0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09,
    ];

    #[test]
    fn test_bypass_decode_matches() {
        let mut cpp = CppCabac::new(TEST_DATA);
        let mut rust = RustCabac::new(TEST_DATA);

        for i in 0..100 {
            let cpp_bit = cpp.decode_bypass();
            let rust_bit = rust.decode_bypass();

            let (cpp_r, cpp_v, cpp_b) = cpp.get_state();
            let (rust_r, rust_v, rust_b) = rust.get_state();

            assert_eq!(cpp_bit, rust_bit,
                "Bit {} mismatch: C++={} Rust={}", i, cpp_bit, rust_bit);
            assert_eq!(cpp_r, rust_r,
                "Range mismatch at bit {}: C++={} Rust={}", i, cpp_r, rust_r);
            assert_eq!(cpp_v, rust_v,
                "Value mismatch at bit {}: C++={} Rust={}", i, cpp_v, rust_v);
            assert_eq!(cpp_b, rust_b,
                "Bits_needed mismatch at bit {}: C++={} Rust={}", i, cpp_b, rust_b);
        }
    }

    #[test]
    fn test_bypass_bits_matches() {
        for num_bits in 1..=8 {
            let mut cpp = CppCabac::new(TEST_DATA);
            let mut rust = RustCabac::new(TEST_DATA);

            for i in 0..10 {
                let cpp_val = cpp.decode_bypass_bits(num_bits);
                let rust_val = rust.decode_bypass_bits(num_bits);

                assert_eq!(cpp_val, rust_val,
                    "Bypass bits mismatch at iteration {}, num_bits={}: C++={} Rust={}",
                    i, num_bits, cpp_val, rust_val);
            }
        }
    }

    /// Simulate sign decoding for a sub-block
    /// Returns the signs decoded and the final state
    fn simulate_sign_decode(cabac: &mut impl CabacLike, num_coeffs: usize, skip_last: bool) -> Vec<u32> {
        let to_decode = if skip_last { num_coeffs.saturating_sub(1) } else { num_coeffs };
        let mut signs = Vec::with_capacity(to_decode);
        for _ in 0..to_decode {
            signs.push(cabac.decode_bypass());
        }
        signs
    }

    trait CabacLike {
        fn decode_bypass(&mut self) -> u32;
        fn get_state(&self) -> (u32, u32, i32);
    }

    impl CabacLike for CppCabac {
        fn decode_bypass(&mut self) -> u32 { CppCabac::decode_bypass(self) }
        fn get_state(&self) -> (u32, u32, i32) { CppCabac::get_state(self) }
    }

    impl<'a> CabacLike for RustCabac<'a> {
        fn decode_bypass(&mut self) -> u32 { RustCabac::decode_bypass(self) }
        fn get_state(&self) -> (u32, u32, i32) { RustCabac::get_state(self) }
    }

    #[test]
    fn test_sign_decode_with_hiding() {
        // Test that skipping the last sign bit causes divergence
        // This simulates what happens with sign_data_hiding

        // Decode signs for 8 coefficients WITHOUT hiding
        let mut cpp_no_hide = CppCabac::new(TEST_DATA);
        let mut rust_no_hide = RustCabac::new(TEST_DATA);

        let cpp_signs = simulate_sign_decode(&mut cpp_no_hide, 8, false);
        let rust_signs = simulate_sign_decode(&mut rust_no_hide, 8, false);

        println!("Without hiding (8 signs): C++={:?} Rust={:?}", cpp_signs, rust_signs);
        assert_eq!(cpp_signs, rust_signs, "Signs should match without hiding");

        let (cpp_r, cpp_v, cpp_b) = cpp_no_hide.get_state();
        let (rust_r, rust_v, rust_b) = rust_no_hide.get_state();
        println!("State after 8 signs: C++=({},{},{}) Rust=({},{},{})", cpp_r, cpp_v, cpp_b, rust_r, rust_v, rust_b);
        assert_eq!((cpp_r, cpp_v, cpp_b), (rust_r, rust_v, rust_b));

        // Now decode signs for 8 coefficients WITH hiding (skip last)
        let mut cpp_hide = CppCabac::new(TEST_DATA);
        let mut rust_hide = RustCabac::new(TEST_DATA);

        let cpp_signs_hide = simulate_sign_decode(&mut cpp_hide, 8, true);
        let rust_signs_hide = simulate_sign_decode(&mut rust_hide, 8, true);

        println!("With hiding (7 signs): C++={:?} Rust={:?}", cpp_signs_hide, rust_signs_hide);
        assert_eq!(cpp_signs_hide, rust_signs_hide, "Signs should match with hiding");

        let (cpp_r, cpp_v, cpp_b) = cpp_hide.get_state();
        let (rust_r, rust_v, rust_b) = rust_hide.get_state();
        println!("State after 7 signs: C++=({},{},{}) Rust=({},{},{})", cpp_r, cpp_v, cpp_b, rust_r, rust_v, rust_b);
        assert_eq!((cpp_r, cpp_v, cpp_b), (rust_r, rust_v, rust_b));

        // The state after hiding should be DIFFERENT from without hiding
        // (one less bit consumed)
        let (no_hide_r, no_hide_v, _) = cpp_no_hide.get_state();
        let (hide_r, hide_v, _) = cpp_hide.get_state();
        println!("\nState comparison:");
        println!("  After 8 signs (no hiding): range={}, value={}", no_hide_r, no_hide_v);
        println!("  After 7 signs (with hiding): range={}, value={}", hide_r, hide_v);
        assert_ne!(no_hide_v, hide_v, "States should differ when hiding one sign");
    }

    #[test]
    fn test_coeff_abs_level_remaining_matches() {
        for rice_param in 0..=4 {
            let mut cpp = CppCabac::new(TEST_DATA);
            let mut rust = RustCabac::new(TEST_DATA);

            for i in 0..5 {
                let (cpp_r, cpp_v, cpp_b) = cpp.get_state();
                let (rust_r, rust_v, rust_b) = rust.get_state();

                println!("Before decode {}, rice={}: C++ state=({},{},{}) Rust state=({},{},{})",
                    i, rice_param, cpp_r, cpp_v, cpp_b, rust_r, rust_v, rust_b);

                let cpp_val = cpp.decode_coeff_abs_level_remaining(rice_param);
                let rust_val = rust.decode_coeff_abs_level_remaining(rice_param);

                println!("  C++ result={}, Rust result={}", cpp_val, rust_val);

                assert_eq!(cpp_val, rust_val,
                    "coeff_abs_level_remaining mismatch at iteration {}, rice_param={}: C++={} Rust={}",
                    i, rice_param, cpp_val, rust_val);

                let (cpp_r, cpp_v, cpp_b) = cpp.get_state();
                let (rust_r, rust_v, rust_b) = rust.get_state();
                assert_eq!((cpp_r, cpp_v, cpp_b), (rust_r, rust_v, rust_b),
                    "State mismatch after decode {}: C++=({},{},{}) Rust=({},{},{})",
                    i, cpp_r, cpp_v, cpp_b, rust_r, rust_v, rust_b);
            }
        }
    }
}
