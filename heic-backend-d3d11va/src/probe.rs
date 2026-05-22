//! Runtime availability probe for D3D11 Video Acceleration HEVC decode.
//!
//! Creates a hardware D3D11 device, queries `ID3D11VideoDevice`, walks
//! the GetVideoDecoderProfile list, and looks for the HEVC Main /
//! Main10 GUID. Returns `true` if all four steps succeed.
//!
//! Failure modes (all reported as `false`, with the parent's dispatcher
//! falling through to the next backend):
//!
//! * Headless / Hyper-V VM with no GPU: `D3D11CreateDevice` returns
//!   `DXGI_ERROR_UNSUPPORTED` against `D3D_DRIVER_TYPE_HARDWARE`.
//! * Old GPU lacking HEVC: `GetVideoDecoderProfile` enumerates a list
//!   that doesn't include the HEVC GUID (e.g. Intel HD 4000, Kepler).
//! * Driver doesn't expose `ID3D11VideoDevice`: `cast` fails.

#![cfg(target_os = "windows")]

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device, ID3D11VideoDevice,
};
use windows::core::{GUID, Interface};

/// HEVC Main (8-bit) decoder profile GUID — `D3D11_DECODER_PROFILE_HEVC_VLD_MAIN`
/// from `dxva.h`. Equal to `{5b11d51b-2f4c-4452-bcc3-09f2a1160cc0}`.
const HEVC_VLD_MAIN: GUID = GUID::from_u128(0x5b11d51b_2f4c_4452_bcc3_09f2a1160cc0);

/// HEVC Main10 (10-bit) decoder profile GUID —
/// `D3D11_DECODER_PROFILE_HEVC_VLD_MAIN10` from `dxva.h`.
/// `{107af0e0-ef1a-4d19-aba8-67a163073d13}`.
const HEVC_VLD_MAIN10: GUID = GUID::from_u128(0x107af0e0_ef1a_4d19_aba8_67a163073d13);

/// Drive the probe. Returns `true` only when every step succeeds.
pub(super) fn probe() -> bool {
    let feature_levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: D3D11CreateDevice is the documented device constructor.
    // Passing None for the adapter selects the default;
    // D3D_DRIVER_TYPE_HARDWARE forces a GPU device (WARP software
    // fallback doesn't support video decode for HEVC). The
    // feature-level array + SDK_VERSION are the canonical recipe.
    let hr = unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    };
    if hr.is_err() {
        return false;
    }
    let Some(device) = device else {
        return false;
    };

    // SAFETY: ID3D11Device::cast queries the COM aggregate for the
    // requested interface and returns Err if not supported.
    let Ok(video_device): Result<ID3D11VideoDevice, _> = device.cast() else {
        return false;
    };

    // Walk the supported-profile array. GetVideoDecoderProfileCount tells
    // us how many; GetVideoDecoderProfile(i) gives us each GUID.
    // SAFETY: documented enumerator on a live ID3D11VideoDevice.
    let count = unsafe { video_device.GetVideoDecoderProfileCount() };
    for i in 0..count {
        // SAFETY: i < count per loop bound; GetVideoDecoderProfile writes
        // the GUID through its out-pointer on success.
        let Ok(guid) = (unsafe { video_device.GetVideoDecoderProfile(i) }) else {
            continue;
        };
        if guid == HEVC_VLD_MAIN || guid == HEVC_VLD_MAIN10 {
            return true;
        }
    }
    false
}
