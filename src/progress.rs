//! Private progress planning. All weights are equal, approximate work shares,
//! not predictions of elapsed time. Only child phases are completed here.

use crate::{HeicError, Result};
use alloc::vec::Vec;
use how_far::{Complete, PhaseSpec, Pulse, Stages, Total};

pub(crate) struct Sequence<'a> {
    pulse: &'a dyn Pulse,
    stages: Option<Stages<'a>>,
}

impl<'a> Sequence<'a> {
    pub(crate) fn new(pulse: &'a dyn Pulse, names: &[&str]) -> Self {
        // Avoid planning allocations for None/NoPulse and StopOnly callers.
        let stages = pulse.may_report().then(|| {
            let specs: Vec<_> = names
                .iter()
                .map(|name| PhaseSpec::new(name, 1, Total::Unknown))
                .collect();
            Stages::new(pulse, &specs)
        });
        Self { pulse, stages }
    }

    pub(crate) fn run<T>(&mut self, work: impl FnOnce(&dyn Pulse) -> Result<T>) -> Result<T> {
        let checked = |pulse: &dyn Pulse| {
            pulse
                .check()
                .map_err(|e| whereat::at!(HeicError::from(e)))?;
            let value = work(pulse)?;
            // Include requests raised by the final report or an indivisible
            // backend call, even when no subsequent phase is needed.
            pulse
                .check()
                .map_err(|e| whereat::at!(HeicError::from(e)))?;
            Ok(value)
        };
        match &mut self.stages {
            Some(stages) => stages.run_classified(is_stopped, checked),
            None => checked(self.pulse),
        }
    }

    pub(crate) fn skip(&mut self) {
        if let Some(stages) = &mut self.stages {
            stages.skip();
        }
    }

    pub(crate) fn complete<T>(self, result: Result<T>) -> Result<T> {
        if let Some(stages) = self.stages {
            stages.complete_classified(result, is_stopped)
        } else {
            result
        }
    }
}

pub(crate) fn total(pulse: &dyn Pulse, units: usize) {
    if pulse.may_report()
        && let Err(error) = pulse.set_total(Total::Exact(units as u64))
    {
        pulse.record_issue(error);
    }
}

fn is_stopped(error: &whereat::At<HeicError>) -> bool {
    matches!(error.error(), HeicError::Cancelled(_))
}
