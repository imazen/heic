//! Private forwarding view. Owned job stop policies remain shareable;
//! a caller's explicitly borrowed stop policy remains bounded by its borrow.
use how_far::{
    Child, Execution, PhaseSpec, PlanError, Pulse, Report, SharedPulse, Stop, StopReason, Total,
    WithStop,
};

pub(crate) enum Observation<'a> {
    Pulse(&'a dyn Pulse),
    Combined(WithStop<'a>),
}
impl Observation<'_> {
    fn inner(&self) -> &dyn Pulse {
        match self {
            Self::Pulse(pulse) => *pulse,
            Self::Combined(pulse) => pulse,
        }
    }
}
impl Stop for Observation<'_> {
    #[track_caller]
    fn check(&self) -> Result<(), StopReason> {
        self.inner().check()
    }
    fn may_stop(&self) -> bool {
        self.inner().may_stop()
    }
}
impl Report for Observation<'_> {
    #[track_caller]
    fn advance(&self, n: u64) {
        self.inner().advance(n)
    }
    fn may_report(&self) -> bool {
        self.inner().may_report()
    }
}
impl Pulse for Observation<'_> {
    #[track_caller]
    fn record_issue(&self, error: PlanError) {
        self.inner().record_issue(error)
    }
    fn split(
        &self,
        execution: Execution,
        parts: &[PhaseSpec<'_>],
    ) -> Result<alloc::vec::Vec<Child<'_>>, PlanError> {
        self.inner().split(execution, parts)
    }
    fn start(&self) -> Result<(), PlanError> {
        self.inner().start()
    }
    fn set_total(&self, total: Total) -> Result<(), PlanError> {
        self.inner().set_total(total)
    }
    fn share(&self) -> Result<SharedPulse, PlanError> {
        self.inner().share()
    }
}

#[cfg(feature = "zencodec")]
pub(crate) fn observation<'a>(
    pulse: Option<&'a SharedPulse>,
    stop: Option<&zencodec::StopToken>,
) -> Observation<'a> {
    let pulse = pulse.map_or(&how_far::NoPulse as &dyn Pulse, |p| p);
    match stop {
        Some(stop) => Observation::Combined(WithStop::new(pulse, stop.clone())),
        None => Observation::Pulse(pulse),
    }
}

pub(crate) fn borrowed<'a>(
    pulse: Option<&'a dyn Pulse>,
    stop: Option<&'a dyn Stop>,
) -> Observation<'a> {
    let pulse = pulse.unwrap_or(&how_far::NoPulse);
    match stop {
        Some(stop) => Observation::Combined(WithStop::borrowed(pulse, stop)),
        None => Observation::Pulse(pulse),
    }
}

#[cfg(all(test, feature = "zencodec"))]
mod tests {
    use super::*;
    use how_far::Complete;
    use how_far_along::{Phase, PulseTree, Status};
    #[test]
    fn owned_job_policy_shares_reporting_planning_and_cancellation() {
        struct Timeout;
        impl Stop for Timeout {
            fn check(&self) -> Result<(), StopReason> {
                Err(StopReason::TimedOut)
            }
        }
        for explicit in [false, true] {
            let pulse = PulseTree::new(Phase::new("job", Total::Unknown), how_far::Unstoppable);
            let shared = pulse.share().unwrap();
            let stop = explicit.then(|| zencodec::StopToken::new(Timeout));
            let view = observation(Some(&shared), stop.as_ref());
            let owned = view.share().expect("owned policies must stay shareable");
            let mut children = owned
                .split(
                    Execution::Sequence,
                    &[PhaseSpec::new("native", 1, Total::Exact(2))],
                )
                .unwrap();
            let child = children.remove(0);
            let worker = child.share().expect("native child keeps owned policy");
            assert_eq!(
                worker.check(),
                if explicit {
                    Err(StopReason::TimedOut)
                } else {
                    Ok(())
                }
            );
            if !explicit {
                worker.advance(2);
            }
            drop(worker);
            child.complete_as(if explicit {
                how_far::Outcome::Cancelled
            } else {
                how_far::Outcome::Succeeded
            });
            let snap = pulse.observer().snapshot();
            assert_eq!(snap.status, Status::Running);
            assert_eq!(snap.children[0].completed, if explicit { 0 } else { 2 });
            drop(owned);
            drop(view);
            drop(shared);
            pulse.complete_as(if explicit {
                how_far::Outcome::Cancelled
            } else {
                how_far::Outcome::Succeeded
            });
        }
    }
}
