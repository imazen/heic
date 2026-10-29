//! Native Pulse migration controls. Source prepared; qualification is separate.
#![cfg(all(feature = "backend-rust", feature = "zencodec"))]

use enough::{Stop, StopReason};
use heic::{HeicDecoderConfig, HeicError};
use how_far::{Complete, Outcome, Pulse, Report, Total};
use how_far_along::{FnPulse, Phase, PulseTree, Status};
use std::borrow::Cow;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use zencodec::decode::{Decode, DecodeJob, DecoderConfig, DynDecoderConfig, StreamingDecode};
use zenpixels::PixelDescriptor;

fn fixture(name: &str) -> Vec<u8> {
    let root = std::env::var_os("HEIC_PULSE_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/features")
        });
    std::fs::read(root.join(format!("{name}.heic"))).unwrap()
}

fn native_timeout(error: &(dyn std::error::Error + 'static)) {
    let native =
        zencodec::find_cause::<HeicError>(error).expect("native cancellation cause retained");
    assert!(matches!(native, HeicError::Cancelled(StopReason::TimedOut)));
}

#[test]
fn shared_job_preserves_pixels_and_native_child_planning_for_u8_and_u16() {
    for (name, descriptor) in [
        ("single", PixelDescriptor::RGB8_SRGB),
        ("depth10", PixelDescriptor::RGB16_SRGB),
    ] {
        let data = fixture(name);
        let config = HeicDecoderConfig::new();
        let baseline = config
            .clone()
            .job()
            .decoder(Cow::Borrowed(&data), &[descriptor])
            .unwrap()
            .decode()
            .unwrap();
        let pulse = PulseTree::new(Phase::new("HEIC", Total::Unknown), how_far::Unstoppable);
        let observer = pulse.observer();
        let job = config.job().with_pulse(pulse.share().unwrap());
        job.probe(&data).unwrap();
        job.output_info(&data).unwrap();
        assert!(
            observer.snapshot().children.is_empty(),
            "preflight leaves native planning available"
        );
        let output = job
            .decoder(Cow::Borrowed(&data), &[descriptor])
            .unwrap()
            .decode()
            .unwrap();
        assert_eq!(
            baseline.pixels().contiguous_bytes(),
            output.pixels().contiguous_bytes()
        );
        assert_eq!(
            (
                baseline.info().width,
                baseline.info().height,
                baseline.info().orientation
            ),
            (
                output.info().width,
                output.info().height,
                output.info().orientation
            )
        );
        assert_eq!(output.pixels().descriptor(), baseline.pixels().descriptor());
        let snap = observer.snapshot();
        assert_eq!(
            snap.status,
            Status::Running,
            "caller alone owns root completion"
        );
        let native = &snap.children[0].children[0].children;
        assert_eq!(
            native.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["Parse HEIF", "Primary image", "Alpha image"]
        );
        assert_eq!(native[0].completed, 1);
        assert_eq!(native[1].completed, 1);
        assert_eq!(native[0].status, Status::Finished(Outcome::Succeeded));
        assert_eq!(native[1].status, Status::Finished(Outcome::Succeeded));
        pulse.complete_as(Outcome::Succeeded);
    }
}

#[test]
fn typed_and_erased_jobs_keep_timeout_from_real_primary_boundary() {
    let data = fixture("single");
    for erased in [false, true] {
        let pulse = FnPulse::new("HEIC", |cx| {
            if cx.phase_name() == "Primary image" {
                Err(StopReason::TimedOut)
            } else {
                Ok(())
            }
        });
        let config = HeicDecoderConfig::new();
        if erased {
            let dyn_config: &dyn DynDecoderConfig = &config;
            let mut job = dyn_config.dyn_job();
            job.set_pulse(pulse.share().unwrap());
            let decoder = job
                .into_decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
                .unwrap();
            let error = decoder.decode().unwrap_err();
            native_timeout(error.as_ref());
        } else {
            let decoder = config
                .job()
                .with_pulse(pulse.share().unwrap())
                .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
                .unwrap();
            let error = decoder.decode().unwrap_err();
            native_timeout(&error);
        }
        let snap = pulse.observer().snapshot();
        let native = &snap.children[0].children[0].children;
        assert_eq!(native[0].status, Status::Finished(Outcome::Succeeded));
        assert_eq!(native[1].status, Status::Finished(Outcome::Cancelled));
        assert_eq!(native[2].status, Status::Finished(Outcome::NotRun));
        pulse.complete_as(Outcome::Cancelled);
    }
}

#[test]
fn pull_session_retains_job_pulse_after_constructor_for_grid_and_fallback() {
    for name in ["single", "grid"] {
        let data = fixture(name);
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        let pulse = FnPulse::new("HEIC session", move |_| {
            if flag.load(Ordering::SeqCst) {
                Err(StopReason::TimedOut)
            } else {
                Ok(())
            }
        });
        let mut decoder = HeicDecoderConfig::new()
            .job()
            .with_pulse(pulse.share().unwrap())
            .streaming_decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
            .unwrap();
        stopped.store(true, Ordering::SeqCst);
        let error = decoder.next_batch().unwrap_err();
        native_timeout(&error);
        drop(decoder);
        pulse.complete_as(Outcome::Cancelled);
    }
}

#[test]
fn rejected_plan_keeps_original_timeout_and_destination_untouched() {
    struct Declines(Arc<AtomicUsize>);
    impl Stop for Declines {
        fn check(&self) -> Result<(), StopReason> {
            if self.0.load(Ordering::SeqCst) > 0 {
                Err(StopReason::TimedOut)
            } else {
                Ok(())
            }
        }
    }
    impl Report for Declines {
        fn advance(&self, _: u64) {}
        fn may_report(&self) -> bool {
            true
        }
    }
    impl Pulse for Declines {
        fn split(
            &self,
            _: how_far::Execution,
            _: &[how_far::PhaseSpec<'_>],
        ) -> Result<Vec<how_far::Child<'_>>, how_far::PlanError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(how_far::PlanError::Unsupported)
        }
    }
    let data = fixture("single");
    let calls = Arc::new(AtomicUsize::new(0));
    let pulse = Declines(calls.clone());
    let mut destination = vec![0xa5; 64 * 64 * 4];
    let before = destination.clone();
    let config = heic::DecoderConfig::new();
    let error = config
        .decode_request(&data)
        .with_pulse(Some(&pulse))
        .decode_into(&mut destination)
        .unwrap_err();
    native_timeout(&error);
    assert!(calls.load(Ordering::SeqCst) > 0);
    assert_eq!(destination, before);
}

#[test]
fn explicit_stop_combines_with_shared_pulse_and_preserves_reason() {
    struct Timeout;
    impl Stop for Timeout {
        fn check(&self) -> Result<(), StopReason> {
            Err(StopReason::TimedOut)
        }
    }
    let data = fixture("single");
    let pulse = PulseTree::new(Phase::new("HEIC", Total::Unknown), how_far::Unstoppable);
    let job = HeicDecoderConfig::new()
        .job()
        .with_pulse(pulse.share().unwrap())
        .with_stop(zencodec::StopToken::new(Timeout));
    let error = job.probe(&data).unwrap_err();
    native_timeout(&error);
    pulse.complete_as(Outcome::Cancelled);
}

#[test]
fn grid_pull_counts_completed_rows_and_preserves_every_pixel() {
    let data = fixture("grid");
    let baseline = HeicDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .unwrap()
        .decode()
        .unwrap();
    let pulse = PulseTree::new(
        Phase::new("grid rows", Total::Unknown),
        how_far::Unstoppable,
    );
    let observer = pulse.observer();
    let mut decoder = HeicDecoderConfig::new()
        .job()
        .with_pulse(pulse.share().unwrap())
        .streaming_decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .unwrap();
    let expected = baseline.pixels();
    let mut rows = 0;
    while let Some((y, batch)) = decoder.next_batch().unwrap() {
        assert_eq!(y, rows);
        for row in 0..batch.rows() {
            assert_eq!(batch.row(row), expected.row(y + row));
        }
        rows += batch.rows();
    }
    assert_eq!(rows, expected.rows());
    let snap = observer.snapshot();
    assert_eq!(snap.total, Total::Exact(u64::from(rows)));
    assert_eq!(snap.completed, u64::from(rows));
    assert_eq!(snap.status, Status::Running);
    drop(decoder);
    pulse.complete_as(Outcome::Succeeded);
}

#[test]
fn grid_report_callback_timeout_refuses_batch_after_actual_work() {
    let data = fixture("grid");
    let pulse = FnPulse::new("grid rows", |cx| {
        if cx.snapshot().completed > 0 {
            Err(StopReason::TimedOut)
        } else {
            Ok(())
        }
    });
    let observer = pulse.observer();
    let mut decoder = HeicDecoderConfig::new()
        .job()
        .with_pulse(pulse.share().unwrap())
        .streaming_decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .unwrap();
    let error = decoder.next_batch().unwrap_err();
    native_timeout(&error);
    assert!(
        observer.snapshot().completed > 0,
        "must reach real converted rows before refusal"
    );
    drop(decoder);
    pulse.complete_as(Outcome::Cancelled);
}

#[test]
fn native_hevc_entry_preserves_first_stop_reason_without_repolling() {
    struct TimeoutOnce(AtomicUsize);
    impl Stop for TimeoutOnce {
        fn check(&self) -> Result<(), StopReason> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(StopReason::TimedOut)
            } else {
                Ok(())
            }
        }
    }
    let data = fixture("single");
    let container = heic::heif::parse(&data, &how_far::Unstoppable).unwrap();
    let item = container.primary_item().unwrap();
    let config = item.hevc_config.as_ref().unwrap();
    let bytes = container.get_item_data(item.id).unwrap();
    let stop = TimeoutOnce(AtomicUsize::new(0));
    let error = heic::hevc::decode_with_config_stop(config, &bytes, &stop).unwrap_err();
    assert!(matches!(
        error.error(),
        heic::HevcError::Cancelled(StopReason::TimedOut)
    ));
    assert_eq!(stop.0.load(Ordering::SeqCst), 1);
}

#[test]
fn push_callback_stop_is_checked_before_output_storage_for_u8_and_u16() {
    use zencodec::decode::{DecodeRowSink, SinkError};
    use zenpixels::PixelSliceMut;
    struct BeginStops {
        flag: Arc<AtomicBool>,
        begins: usize,
    }
    impl DecodeRowSink for BeginStops {
        fn begin(&mut self, _: u32, _: u32, _: PixelDescriptor) -> Result<(), SinkError> {
            self.begins += 1;
            self.flag.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn provide_next_buffer(
            &mut self,
            _: u32,
            _: u32,
            _: u32,
            _: PixelDescriptor,
        ) -> Result<PixelSliceMut<'_>, SinkError> {
            panic!("sink requested cancellation at begin; no subsequent storage effect allowed")
        }
        fn finish(&mut self) -> Result<(), SinkError> {
            panic!("cancelled push must not finish output")
        }
    }
    let data = fixture("depth10");
    for descriptor in [PixelDescriptor::RGB8_SRGB, PixelDescriptor::RGB16_SRGB] {
        let flag = Arc::new(AtomicBool::new(false));
        let check = flag.clone();
        let pulse = FnPulse::new("push", move |_| {
            if check.load(Ordering::SeqCst) {
                Err(StopReason::TimedOut)
            } else {
                Ok(())
            }
        });
        let mut sink = BeginStops { flag, begins: 0 };
        let error = HeicDecoderConfig::new()
            .job()
            .with_pulse(pulse.share().unwrap())
            .push_decoder(Cow::Borrowed(&data), &mut sink, &[descriptor])
            .unwrap_err();
        native_timeout(&error);
        assert_eq!(sink.begins, 1);
        pulse.complete_as(Outcome::Cancelled);
    }
}

#[test]
fn optional_native_pulse_preserves_pixels_and_none_keeps_explicit_stop() {
    struct Timeout;
    impl Stop for Timeout {
        fn check(&self) -> Result<(), StopReason> {
            Err(StopReason::TimedOut)
        }
    }
    let config = heic::DecoderConfig::new();
    for name in ["single", "depth10", "grid"] {
        let data = fixture(name);
        let baseline = config.decode_request(&data).decode().unwrap();
        for pulse in [None, Some(&how_far::NoPulse as &dyn Pulse)] {
            let output = config
                .decode_request(&data)
                .with_pulse(pulse)
                .decode()
                .unwrap();
            assert_eq!(output.data, baseline.data);
            assert_eq!(
                (output.width, output.height),
                (baseline.width, baseline.height)
            );
        }
        let mut destination = vec![0xa5; baseline.data.len()];
        let before = destination.clone();
        let error = config
            .decode_request(&data)
            .with_stop(&Timeout)
            .with_pulse(Some(&how_far::NoPulse))
            .with_pulse(None)
            .decode_into(&mut destination)
            .unwrap_err();
        native_timeout(&error);
        assert_eq!(destination, before);
    }
}
