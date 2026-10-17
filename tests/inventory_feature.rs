#![cfg(feature = "zencodec")]

use heic::HeicDecoderConfig;
use zencodec::decode::{DecodeJob, DecoderConfig, DynDecoderConfig};
use zencodec::{CodecErrorExt, UnsupportedOperation};

#[test]
fn capability_matches_compiled_inventory_and_both_dispatch_paths() {
    let config = HeicDecoderConfig::new();
    assert_eq!(
        <HeicDecoderConfig as DecoderConfig>::capabilities().inventory(),
        cfg!(feature = "inventory")
    );
    let generic = config.clone().job().inventory(b"malformed");
    let dynamic: &dyn DynDecoderConfig = &config;
    let erased = dynamic.dyn_job().inventory(b"malformed");
    if cfg!(feature = "inventory") {
        let inventory = generic.unwrap();
        inventory.validate().unwrap();
        assert_eq!(inventory, erased.unwrap());
    } else {
        assert_eq!(
            generic.unwrap_err().unsupported_operation(),
            Some(&UnsupportedOperation::Inventory)
        );
        assert_eq!(
            erased.unwrap_err().unsupported_operation(),
            Some(&UnsupportedOperation::Inventory)
        );
    }
}

#[cfg(feature = "inventory")]
#[test]
fn configured_limits_and_cancellation_survive_dispatch() {
    use zencodec::decode::DynDecoderConfig;
    use zencodec::{CategorizedError, CodecErrorExt, ErrorCategory, LimitExceeded, ResourceLimits};
    let data = [0xff, 0xd8, 0xff, 0xd9];
    let config = HeicDecoderConfig::new();
    let limits = ResourceLimits::none().with_max_input_bytes(1);
    let native = config
        .clone()
        .job()
        .with_limits(limits)
        .inventory(&data)
        .unwrap_err();
    let dynamic: &dyn DynDecoderConfig = &config;
    let mut erased_job = dynamic.dyn_job();
    erased_job.set_limits(limits);
    let erased = erased_job.inventory(&data).unwrap_err();
    let expected = LimitExceeded::InputSize {
        actual: data.len() as u64,
        max: 1,
    };
    assert_eq!(native.limit_exceeded(), Some(&expected));
    assert_eq!(erased.limit_exceeded(), Some(&expected));

    struct Cancelled;
    impl enough::Stop for Cancelled {
        fn check(&self) -> Result<(), enough::StopReason> {
            Err(enough::StopReason::Cancelled)
        }
    }
    let stop = zencodec::StopToken::new(Cancelled);
    let native = config
        .clone()
        .job()
        .with_stop(stop.clone())
        .inventory(&data)
        .unwrap_err();
    let mut erased_job = dynamic.dyn_job();
    erased_job.set_stop(stop);
    let erased = erased_job.inventory(&data).unwrap_err();
    assert_eq!(
        native.category(),
        ErrorCategory::Stopped(enough::StopReason::Cancelled)
    );
    assert_eq!(erased.error_category(), Some(native.category()));
}

#[cfg(feature = "inventory")]
#[test]
fn mid_walk_timeout_preserves_first_reason_in_both_dispatch_paths() {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use zencodec::{CategorizedError, ErrorCategory};
    struct TimeoutOnce(AtomicUsize);
    impl enough::Stop for TimeoutOnce {
        fn check(&self) -> Result<(), enough::StopReason> {
            if self.0.fetch_add(1, Ordering::Relaxed) == 1 {
                Err(enough::StopReason::TimedOut)
            } else {
                Ok(())
            }
        }
    }
    let data = [0, 0, 0, 8, b'f', b'r', b'e', b'e'].repeat(4);
    let config = HeicDecoderConfig::new();
    let generic = config
        .clone()
        .job()
        .with_stop(zencodec::StopToken::new(TimeoutOnce(AtomicUsize::new(0))))
        .inventory(&data)
        .unwrap_err();
    let dynamic: &dyn DynDecoderConfig = &config;
    let mut job = dynamic.dyn_job();
    job.set_stop(zencodec::StopToken::new(TimeoutOnce(AtomicUsize::new(0))));
    let erased = job.inventory(&data).unwrap_err();
    let expected = ErrorCategory::Stopped(enough::StopReason::TimedOut);
    assert_eq!(generic.category(), expected);
    assert_eq!(erased.error_category(), Some(expected));
}
