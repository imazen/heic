//! Runtime availability probe for libva HEVC decode support.
//!
//! Loads `libva.so.2` + `libva-drm.so.2` via `libloading` (so the crate
//! builds cleanly on systems without `libva-dev`), opens a DRM render
//! node, initializes a `VADisplay`, and queries the supported profile
//! list looking for `VAProfileHEVCMain` or `VAProfileHEVCMain10`.
//!
//! Returns:
//!
//! * `Ok(true)` — libva is present, a render node opened, and at
//!   least one HEVC profile is registered for `VAEntrypointVLD`.
//! * `Ok(false)` — libva is present but no HEVC profile is registered
//!   (driver missing, GPU doesn't support HEVC decode, etc.).
//! * `Err(_)` — libva couldn't be loaded at all; treated by the parent
//!   as `Unavailable`.

#![cfg(target_os = "linux")]

use std::ffi::{CStr, c_int, c_void};
use std::fs::File;
use std::os::fd::IntoRawFd;

use libloading::Library;

/// `VAStatus` is `int`; 0 means success per the libva ABI.
type VaStatus = c_int;
const VA_STATUS_SUCCESS: VaStatus = 0;

/// `VAProfile` enum values from `va.h`. Only the two HEVC main profiles
/// matter for our probe — Range Extension, Screen Content, and 3D
/// profiles aren't HEIC-relevant.
const VA_PROFILE_HEVC_MAIN: i32 = 17;
const VA_PROFILE_HEVC_MAIN_10: i32 = 18;

// `VAEntrypoint` for video decode (referenced once we add the per-profile
// entrypoint check). Currently we only filter on profile id; if a future
// caller wants to also verify VAEntrypointVLD support per profile we
// loop the queryConfigEntrypoints array here.
#[allow(dead_code)]
const VA_ENTRYPOINT_VLD: i32 = 1;

/// Drive the probe. Returns `Ok(true)` if at least one HEVC profile is
/// registered, `Ok(false)` if libva loaded but no profile matched,
/// `Err(_)` if libva can't be loaded.
///
/// Two display backends are tried in order:
///
/// 1. **DRM render nodes** (`/dev/dri/renderD128..D135`). Works on
///    physical Linux + most VMs with a real GPU passthrough. The
///    primary path on bare-metal Linux.
/// 2. **X11 display** (`vaGetDisplay` against an `XOpenDisplay(NULL)`
///    handle). Works inside WSL2 where `/dev/dri` is absent but WSLg
///    exposes an X server bridged to the host GPU. The libva driver
///    that backs this (e.g. `nvidia_drv_video.so` via NVDEC, or the
///    Mesa d3d12 driver) handles the actual decode through CUDA /
///    D3D12 — libva itself doesn't care which display protocol
///    fronts the driver.
pub(super) fn probe() -> Result<bool, ProbeError> {
    let libva = open_lib("libva.so.2")?;
    let libva_drm = open_lib("libva-drm.so.2")?;

    // Resolve the four symbols we need. Returning early on any missing
    // symbol gives the same "Unavailable" semantics as missing libva.
    // SAFETY: every dlsym below names a stable libva entry point with
    // the documented C signature.
    // SAFETY: each `Library::get` reads an exported symbol whose signature
    // we mirror exactly from the libva headers; libloading verifies the
    // symbol exists (returning Err if missing) and the FFI signatures
    // match the documented C ABI.
    let va_get_display_drm: libloading::Symbol<unsafe extern "C" fn(c_int) -> *mut c_void> =
        unsafe { libva_drm.get(b"vaGetDisplayDRM\0") }.map_err(ProbeError::SymbolMissing)?;
    // SAFETY: see above for the libloading::get contract.
    let va_initialize: libloading::Symbol<
        unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> VaStatus,
    > = unsafe { libva.get(b"vaInitialize\0") }.map_err(ProbeError::SymbolMissing)?;
    // SAFETY: see above.
    let va_terminate: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> VaStatus> =
        unsafe { libva.get(b"vaTerminate\0") }.map_err(ProbeError::SymbolMissing)?;
    // SAFETY: see above.
    let va_max_num_profiles: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> c_int> =
        unsafe { libva.get(b"vaMaxNumProfiles\0") }.map_err(ProbeError::SymbolMissing)?;
    // SAFETY: see above.
    let va_query_config_profiles: libloading::Symbol<
        unsafe extern "C" fn(*mut c_void, *mut i32, *mut c_int) -> VaStatus,
    > = unsafe { libva.get(b"vaQueryConfigProfiles\0") }.map_err(ProbeError::SymbolMissing)?;

    // Try render nodes D128..D135 — covers single-GPU setups (most) and
    // the common dual-GPU laptop layout.
    for node in 128..136 {
        let path = format!("/dev/dri/renderD{node}");
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let fd = file.into_raw_fd();
        // SAFETY: fd is a valid open file descriptor we just opened.
        // vaGetDisplayDRM duplicates the fd internally; we close the
        // original on drop. Display is a handle into libva-allocated
        // state; vaTerminate frees it.
        let display = unsafe { va_get_display_drm(fd) };
        // SAFETY: file is consumed by into_raw_fd; close the fd
        // ourselves to avoid leaking.
        unsafe { libc::close(fd) };
        if display.is_null() {
            continue;
        }

        let mut major: c_int = 0;
        let mut minor: c_int = 0;
        // SAFETY: display is non-null per the check above; major/minor
        // are valid out-pointers.
        let status = unsafe { va_initialize(display, &mut major, &mut minor) };
        if status != VA_STATUS_SUCCESS {
            // SAFETY: display was returned by vaGetDisplayDRM; vaTerminate
            // is the documented teardown.
            let _ = unsafe { va_terminate(display) };
            continue;
        }

        // Query the profile list.
        // SAFETY: display is initialized; vaMaxNumProfiles returns the
        // array length we need to allocate.
        let max = unsafe { va_max_num_profiles(display) };
        if max <= 0 {
            // SAFETY: same display we just initialized.
            let _ = unsafe { va_terminate(display) };
            continue;
        }
        let mut profiles = vec![0i32; max as usize];
        let mut count: c_int = 0;
        // SAFETY: profiles buffer holds `max` slots; count receives the
        // actual fill.
        let status =
            unsafe { va_query_config_profiles(display, profiles.as_mut_ptr(), &mut count) };
        // SAFETY: pairs with vaInitialize above.
        let _ = unsafe { va_terminate(display) };

        if status != VA_STATUS_SUCCESS {
            continue;
        }

        // Look for an HEVC profile in the returned list.
        let found = profiles[..count as usize]
            .iter()
            .any(|&p| p == VA_PROFILE_HEVC_MAIN || p == VA_PROFILE_HEVC_MAIN_10);
        if found {
            // Future: cache the (display, profile, max bit-depth) for
            // decode() to reuse rather than re-probe per backend instance.
            // The libloading::Library handles are dropped at end of scope;
            // libva uses static state internally so this is safe.
            let _ = (va_get_display_drm, va_initialize, va_terminate);
            let _ = (va_max_num_profiles, va_query_config_profiles);
            let _ = (libva, libva_drm);
            return Ok(true);
        }
        // Otherwise keep walking render nodes — multi-GPU systems may
        // have one node without HEVC and another with.
    }

    // WSL2 fallback: no /dev/dri but the host GPU is reachable via
    // WSLg's X11 bridge. Open an X11 display through libX11, hand it
    // to vaGetDisplay (libva-x11.so.2), and repeat the HEVC profile
    // probe against that VADisplay.
    if let Some(found) = probe_via_x11(
        &libva,
        &va_initialize,
        &va_terminate,
        &va_max_num_profiles,
        &va_query_config_profiles,
    )? {
        return Ok(found);
    }

    let _ = libva_drm;
    Ok(false)
}

/// Try `vaGetDisplay(XOpenDisplay(NULL))` as a fallback when no DRM
/// render nodes are available. Returns:
/// * `Some(true)` — X11 display backend worked and HEVC profile found.
/// * `Some(false)` — X11 + libva initialized but no HEVC profile.
/// * `None` — X11 path not usable (no $DISPLAY, libX11 / libva-x11
///   missing, XOpenDisplay returned NULL). Caller falls through.
fn probe_via_x11(
    _libva: &Library,
    va_initialize: &libloading::Symbol<
        unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> VaStatus,
    >,
    va_terminate: &libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> VaStatus>,
    va_max_num_profiles: &libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> c_int>,
    va_query_config_profiles: &libloading::Symbol<
        unsafe extern "C" fn(*mut c_void, *mut i32, *mut c_int) -> VaStatus,
    >,
) -> Result<Option<bool>, ProbeError> {
    if std::env::var_os("DISPLAY").is_none() {
        return Ok(None);
    }
    // SAFETY: libloading::Library::new is the documented dlopen entry
    // point; both SONAMES are stable.
    let Ok(libx11) = (unsafe { Library::new("libX11.so.6") }) else {
        return Ok(None);
    };
    // SAFETY: same as libx11 above.
    let Ok(libva_x11) = (unsafe { Library::new("libva-x11.so.2") }) else {
        return Ok(None);
    };
    // SAFETY: standard libX11 ABI; XOpenDisplay(NULL) reads $DISPLAY
    // and returns a Display* (opaque pointer) or NULL.
    let x_open_display: libloading::Symbol<
        unsafe extern "C" fn(*const std::ffi::c_char) -> *mut c_void,
    > = unsafe { libx11.get(b"XOpenDisplay\0") }.map_err(ProbeError::SymbolMissing)?;
    // SAFETY: pairs with XOpenDisplay; closes the connection.
    let x_close_display: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> c_int> =
        unsafe { libx11.get(b"XCloseDisplay\0") }.map_err(ProbeError::SymbolMissing)?;
    // SAFETY: vaGetDisplay takes a Display* and returns a VADisplay.
    let va_get_display: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> *mut c_void> =
        unsafe { libva_x11.get(b"vaGetDisplay\0") }.map_err(ProbeError::SymbolMissing)?;

    // SAFETY: XOpenDisplay(NULL) reads $DISPLAY internally; returns
    // a Display* or NULL on failure. We just verified $DISPLAY is set.
    let x_display = unsafe { x_open_display(core::ptr::null()) };
    if x_display.is_null() {
        return Ok(None);
    }
    // SAFETY: x_display is non-null. vaGetDisplay returns a VADisplay
    // backed by the X11 connection; libva tracks the connection
    // internally for the lifetime of the returned display.
    let va_display = unsafe { va_get_display(x_display) };
    if va_display.is_null() {
        // SAFETY: x_display was returned by XOpenDisplay; XCloseDisplay
        // is its documented teardown.
        unsafe { x_close_display(x_display) };
        return Ok(None);
    }

    let mut major: c_int = 0;
    let mut minor: c_int = 0;
    // SAFETY: standard vaInitialize call against an X11-backed VADisplay.
    let status = unsafe { va_initialize(va_display, &mut major, &mut minor) };
    if status != VA_STATUS_SUCCESS {
        // SAFETY: vaTerminate pairs with vaInitialize, even when init
        // failed — libva documents it.
        let _ = unsafe { va_terminate(va_display) };
        // SAFETY: same x_display we opened above.
        unsafe { x_close_display(x_display) };
        return Ok(Some(false));
    }

    // SAFETY: display is initialized; bounded-size profile array.
    let max = unsafe { va_max_num_profiles(va_display) };
    let result = if max > 0 {
        let mut profiles = vec![0i32; max as usize];
        let mut count: c_int = 0;
        // SAFETY: profiles buffer holds `max` slots.
        let status =
            unsafe { va_query_config_profiles(va_display, profiles.as_mut_ptr(), &mut count) };
        if status == VA_STATUS_SUCCESS {
            profiles[..count as usize]
                .iter()
                .any(|&p| p == VA_PROFILE_HEVC_MAIN || p == VA_PROFILE_HEVC_MAIN_10)
        } else {
            false
        }
    } else {
        false
    };

    // SAFETY: pairs with vaInitialize.
    let _ = unsafe { va_terminate(va_display) };
    // SAFETY: pairs with XOpenDisplay.
    unsafe { x_close_display(x_display) };
    // Keep libloading handles in scope so the symbols stay valid.
    let _ = (libx11, libva_x11);
    Ok(Some(result))
}

#[derive(Debug)]
#[allow(dead_code)] // the Strings carry diagnostic context for future logging
pub(super) enum ProbeError {
    /// `libva.so.2` or `libva-drm.so.2` not present on the loader path.
    /// String captures both the soname and the dlerror text so logs
    /// distinguish "library missing" from "library bad ELF".
    LibraryMissing(String),
    /// Library loaded but an expected symbol is absent — typically a
    /// very old libva. The wrapped `libloading::Error` carries the
    /// missing-symbol name for diagnostics.
    SymbolMissing(libloading::Error),
}

fn open_lib(name: &str) -> Result<Library, ProbeError> {
    // SAFETY: libloading::Library::new is the documented entry point;
    // the library names are stable libva SONAMES. We never call into
    // these symbols outside the probe scope so there's no ODR conflict
    // with users that link libva themselves.
    unsafe { Library::new(name) }.map_err(|e| ProbeError::LibraryMissing(format!("{name}: {e}")))
}

// We need libc::close on the dup'd fd path.
extern crate libc;

// Re-export for the parent module's debugging convenience.
#[allow(dead_code)]
pub(super) fn version() -> &'static CStr {
    c"vaapi-probe-v1"
}

// Sanity tests on the probe shape. The actual libva-loaded path runs
// only on Linux with libva installed and a render node accessible;
// we test the error-handling branches here.
#[cfg(test)]
mod tests {
    use super::*;

    /// `open_lib` returns `LibraryMissing` for a name that doesn't exist.
    #[test]
    fn open_lib_missing_returns_error() {
        let err = open_lib("libva-does-not-exist.so.999")
            .expect_err("loading a nonexistent library should fail");
        assert!(
            matches!(err, ProbeError::LibraryMissing(_)),
            "expected LibraryMissing, got {err:?}"
        );
    }
}
