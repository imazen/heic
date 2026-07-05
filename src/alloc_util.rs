//! Allocation helpers honoring an [`AllocPreference`] policy per call site.
//!
//! A HEIC decode mixes two allocation regimes:
//!
//! * **Big, untrusted-sized buffers** (the full-image alpha / gain-map /
//!   depth-map planes, the concatenated AV1 OBU payload, the `unci`
//!   decompressed surface) default to the *fallible* `try_reserve` path — a
//!   crafted container can demand gigabytes, so we want a graceful
//!   [`HeicError::OutOfMemory`] rather than an abort.
//! * **Small, bounded scratch** (a per-tile offset list, sized by the tile
//!   count which the resource limits already cap) defaults to the *infallible*
//!   `Vec::with_capacity` path — a single allocation is faster and the size is
//!   bounded, not unboundedly attacker-controlled.
//!
//! [`AllocPreference`] is a **3-mode, per-site override** of that default:
//! [`Fallible`](AllocPreference::Fallible) /
//! [`Infallible`](AllocPreference::Infallible) force one path everywhere;
//! [`CodecDefault`](AllocPreference::CodecDefault) (and any future
//! `#[non_exhaustive]` variant) keeps each site's own default. The helper
//! signatures therefore take the caller's preference *and* the site default,
//! and resolve them together.
//!
//! ## Why a local enum
//!
//! The cross-codec policy type is [`zencodec::AllocPreference`], but `zencodec`
//! is an **optional** dependency of this crate — the core decode pipeline in
//! [`crate::decode`] compiles without it. So the policy that travels with
//! [`crate::Limits`] through that pipeline is this always-present mirror enum.
//! The `zencodec` adapter ([`crate::codec`]) converts
//! `zencodec::AllocPreference` into this at the decode boundary (see
//! [`AllocPreference::from_zencodec`]).

use alloc::vec::Vec;
use whereat::{At, at};

use crate::error::HeicError;

/// Per-site allocation-fallibility policy (a local mirror of
/// [`zencodec::AllocPreference`]).
///
/// Carried on [`crate::Limits`] so the policy travels with the rest of the
/// resource governance the decoder already threads. `Copy` + `Default`
/// (`CodecDefault`) so it slots into the existing `Limits` plumbing with no
/// behaviour change for callers that never set it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
// `Fallible`/`Infallible` are only constructed via the zencodec `From` impl below
// (and tests); without the `zencodec` feature they're reachable API but unbuilt.
#[cfg_attr(not(feature = "zencodec"), allow(dead_code))]
pub(crate) enum AllocPreference {
    /// Let each call site decide. Big untrusted-sized buffers favour the
    /// fallible path; small bounded scratch favours the infallible path.
    /// Default — preserves existing behaviour.
    #[default]
    CodecDefault,
    /// Force the fallible path: `try_reserve`, returning a graceful
    /// [`HeicError::OutOfMemory`] instead of aborting. Prefer for untrusted
    /// input.
    Fallible,
    /// Force the infallible path: `vec!` / `Vec::with_capacity` (faster — a
    /// single allocation) at the cost of aborting on OOM. Prefer for trusted
    /// sizes and benchmarks.
    Infallible,
}

impl AllocPreference {
    /// Map a [`zencodec::AllocPreference`] onto this crate's mirror enum.
    ///
    /// Used at the `zencodec` decode boundary; `#[cfg]`-gated to the same
    /// feature as the adapter so the core build never references `zencodec`.
    #[cfg(feature = "zencodec")]
    #[inline]
    #[must_use]
    pub(crate) fn from_zencodec(pref: zencodec::AllocPreference) -> Self {
        match pref {
            zencodec::AllocPreference::Fallible => Self::Fallible,
            zencodec::AllocPreference::Infallible => Self::Infallible,
            // CodecDefault and any future #[non_exhaustive] variant → default.
            _ => Self::CodecDefault,
        }
    }
}

/// Resolve the 3-mode [`AllocPreference`] against THIS site's default
/// fallibility.
///
/// * [`Fallible`](AllocPreference::Fallible) → always `true`.
/// * [`Infallible`](AllocPreference::Infallible) → always `false`.
/// * [`CodecDefault`](AllocPreference::CodecDefault) (and any future
///   `#[non_exhaustive]` variant) → the site default, unchanged.
#[inline]
#[must_use]
pub(crate) fn resolve_fallible(pref: AllocPreference, site_default_fallible: bool) -> bool {
    match pref {
        AllocPreference::Fallible => true,
        AllocPreference::Infallible => false,
        _ => site_default_fallible,
    }
}

/// Allocate `n` elements of `T::default()`, honoring the per-site fallibility.
///
/// `pref` is the caller's [`AllocPreference`]; `site_default_fallible` is this
/// site's default when `pref` is `CodecDefault`.
///
/// * fallible → `try_reserve_exact` then fill, returning
///   [`HeicError::OutOfMemory`] on allocation failure.
/// * infallible → `vec![T::default(); n]` (single allocation, aborts on OOM).
///
/// Gated to `unci`: the only pre-filled untrusted buffer is the `unci`
/// decompressed surface ([`crate::decode`]). The capacity-only sites use
/// [`vec_with_capacity`] instead, so this helper has no caller when `unci` is
/// off.
#[cfg(feature = "unci")]
pub(crate) fn alloc_filled<T: Clone + Default>(
    pref: AllocPreference,
    site_default_fallible: bool,
    n: usize,
) -> Result<Vec<T>, At<HeicError>> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(n)
            .map_err(|_| at!(HeicError::OutOfMemory))?;
        v.resize(n, T::default());
        Ok(v)
    } else {
        Ok(alloc::vec![T::default(); n])
    }
}

/// Allocate an empty `Vec<T>` with reserved capacity for `cap` elements,
/// honoring the per-site fallibility (for the `Vec::with_capacity` + push/extend
/// sites).
///
/// `pref` is the caller's [`AllocPreference`]; `site_default_fallible` is this
/// site's default when `pref` is `CodecDefault`.
///
/// * fallible → `try_reserve_exact`, returning [`HeicError::OutOfMemory`] on
///   allocation failure.
/// * infallible → `Vec::with_capacity(cap)` (aborts on OOM).
///
/// The returned `Vec` is empty (length 0); the caller fills it.
pub(crate) fn vec_with_capacity<T>(
    pref: AllocPreference,
    site_default_fallible: bool,
    cap: usize,
) -> Result<Vec<T>, At<HeicError>> {
    if resolve_fallible(pref, site_default_fallible) {
        let mut v = Vec::new();
        v.try_reserve_exact(cap)
            .map_err(|_| at!(HeicError::OutOfMemory))?;
        Ok(v)
    } else {
        Ok(Vec::with_capacity(cap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `CodecDefault` keeps each site's own default fallibility.

    #[test]
    fn codec_default_keeps_site_default_true() {
        // Big-buffer site (default fallible): CodecDefault stays fallible.
        assert!(resolve_fallible(AllocPreference::CodecDefault, true));
    }

    #[test]
    fn codec_default_keeps_site_default_false() {
        // Small-scratch site (default infallible): CodecDefault stays infallible.
        assert!(!resolve_fallible(AllocPreference::CodecDefault, false));
    }

    #[test]
    fn explicit_fallible_overrides_any_site_default() {
        assert!(resolve_fallible(AllocPreference::Fallible, false));
        assert!(resolve_fallible(AllocPreference::Fallible, true));
    }

    #[test]
    fn explicit_infallible_overrides_any_site_default() {
        assert!(!resolve_fallible(AllocPreference::Infallible, true));
        assert!(!resolve_fallible(AllocPreference::Infallible, false));
    }

    #[cfg(feature = "unci")]
    #[test]
    fn alloc_filled_all_modes_equal_bytes() {
        let a = alloc_filled::<u8>(AllocPreference::CodecDefault, true, 4096).unwrap();
        let b = alloc_filled::<u8>(AllocPreference::Infallible, true, 4096).unwrap();
        let c = alloc_filled::<u8>(AllocPreference::Fallible, false, 4096).unwrap();
        assert_eq!(a.len(), 4096);
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert!(a.iter().all(|&x| x == 0));
    }

    #[cfg(feature = "unci")]
    #[test]
    fn alloc_filled_u16_zeroes() {
        let v = alloc_filled::<u16>(AllocPreference::Fallible, true, 64).unwrap();
        assert_eq!(v.len(), 64);
        assert!(v.iter().all(|&x| x == 0));
    }

    #[test]
    fn vec_with_capacity_reserves_and_is_empty() {
        let a = vec_with_capacity::<u8>(AllocPreference::Infallible, false, 1024).unwrap();
        let b = vec_with_capacity::<u16>(AllocPreference::Fallible, false, 1024).unwrap();
        assert_eq!(a.len(), 0);
        assert_eq!(b.len(), 0);
        assert!(a.capacity() >= 1024);
        assert!(b.capacity() >= 1024);
    }

    #[cfg(feature = "unci")]
    #[test]
    fn alloc_filled_fallible_oom_returns_err() {
        // Request a byte count that exceeds `isize::MAX` on every target
        // (32-bit and 64-bit alike): Rust's allocator rejects any request
        // over that bound with a capacity-overflow error before it ever
        // touches the OS allocator, so this is Err deterministically rather
        // than depending on how much free virtual address space the host
        // happens to have. `usize::MAX / 2` (~2 GiB on i686) used to be used
        // here, but 2 GiB is well within reservable virtual address space on
        // 32-bit Linux (it only *reserves*, never *commits*, the memory), so
        // it flaked green on i686 CI instead of erroring.
        let r = alloc_filled::<u8>(AllocPreference::Fallible, true, usize::MAX);
        assert!(r.is_err());
        assert!(matches!(r.unwrap_err().error(), HeicError::OutOfMemory));
    }

    #[test]
    fn vec_with_capacity_fallible_oom_returns_err() {
        // See `alloc_filled_fallible_oom_returns_err` above: `usize::MAX`
        // (not `usize::MAX / 2`) is required to guarantee a capacity-overflow
        // Err on 32-bit targets, where `usize::MAX / 2` (~2 GiB) is
        // reservable virtual address space and does not fail (this test was
        // red on i686 CI with the halved value).
        let r = vec_with_capacity::<u8>(AllocPreference::Fallible, true, usize::MAX);
        assert!(r.is_err());
        assert!(matches!(r.unwrap_err().error(), HeicError::OutOfMemory));
    }

    #[cfg(feature = "zencodec")]
    #[test]
    fn from_zencodec_maps_all_three_modes() {
        assert_eq!(
            AllocPreference::from_zencodec(zencodec::AllocPreference::Fallible),
            AllocPreference::Fallible
        );
        assert_eq!(
            AllocPreference::from_zencodec(zencodec::AllocPreference::Infallible),
            AllocPreference::Infallible
        );
        assert_eq!(
            AllocPreference::from_zencodec(zencodec::AllocPreference::CodecDefault),
            AllocPreference::CodecDefault
        );
    }
}
