//! Fixed-size diagnostics only; never a deadline, authority or replay input.
use serde_json::{json, Value};
use std::{
    cell::{Cell, RefCell},
    fmt,
    time::Instant,
};

const MAX_MS: u128 = 60_000;

#[derive(Clone, Copy)]
pub(in crate::desktop) enum Step {
    NativeRechecks,
    MetadataSync,
    InstanceStop,
    PhysicalExit,
}

#[derive(Clone, Copy, Default)]
struct Sample {
    calls: u32,
    nanos: u128,
}

pub(in crate::desktop) struct Timing {
    start: Instant,
    last: Cell<Instant>,
    active: Cell<Option<Step>>,
    samples: RefCell<[Sample; 4]>,
}

impl Timing {
    pub fn new() -> Self {
        let start = Instant::now();
        Self {
            start,
            last: Cell::new(start),
            active: Cell::new(None),
            samples: RefCell::default(),
        }
    }

    fn transition(&self, next: Option<Step>, now: Instant) -> Option<Step> {
        let previous = self.active.replace(next);
        let elapsed = now.saturating_duration_since(self.last.replace(now));
        let mut samples = self.samples.borrow_mut();
        if let Some(step) = previous {
            let sample = &mut samples[step as usize];
            sample.nanos = sample.nanos.saturating_add(elapsed.as_nanos());
        }
        previous
    }

    pub fn measure<T>(&self, step: Step, operation: impl FnOnce() -> T) -> T {
        let previous = self.transition(Some(step), Instant::now());
        {
            let mut samples = self.samples.borrow_mut();
            let sample = &mut samples[step as usize];
            sample.calls = sample.calls.saturating_add(1);
        }
        struct Scope<'a>(&'a Timing, Option<Step>);
        impl Drop for Scope<'_> {
            fn drop(&mut self) {
                self.0.transition(self.1, Instant::now());
            }
        }
        let _scope = Scope(self, previous);
        operation()
    }

    pub fn snapshot(&self) -> Value {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&self, now: Instant) -> Value {
        let samples = self.samples.borrow();
        let ms = |nanos: u128| (nanos / 1_000_000).min(MAX_MS) as u64;
        let sample = |step: Step| {
            let value = samples[step as usize];
            json!({"calls":value.calls,"elapsed_ms":ms(value.nanos)})
        };
        let elapsed = now.saturating_duration_since(self.start);
        let measured = samples.iter().fold(0u128, |n, s| n.saturating_add(s.nanos));
        json!({"abi":"desktop-mode-stop-timing-v1",
            "budget_ms":super::Budget::Stop.duration().as_millis() as u64,
            "elapsed_ms":ms(elapsed.as_nanos()),
            "unattributed_ms":ms(elapsed.as_nanos().saturating_sub(measured)),
            "steps":{
                "native_rechecks":sample(Step::NativeRechecks),
                "metadata_sync":sample(Step::MetadataSync),
                "instance_stop":sample(Step::InstanceStop),
                "physical_exit":sample(Step::PhysicalExit)}})
    }

    pub fn failure(&self, cause: anyhow::Error) -> anyhow::Error {
        anyhow::Error::new(Failure {
            cause,
            timing: self.snapshot(),
        })
    }
}

#[derive(Debug)]
struct Failure {
    cause: anyhow::Error,
    timing: Value,
}
impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.cause, f)
    }
}
impl std::error::Error for Failure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // Display already prints the cause; returning the cause itself made
        // anyhow's alternate format repeat it ("deadline reached: deadline
        // reached"). Expose the cause's own source instead, so an outer
        // context keeps the exact chain without a duplicate link.
        let cause: &(dyn std::error::Error + 'static) = self.cause.as_ref();
        cause.source()
    }
}

/// Preserve the existing refusal wording; diagnostics are a separate field.
pub(in crate::desktop) fn error_detail(error: &anyhow::Error) -> Value {
    if let Some(failure) = error.downcast_ref::<Failure>() {
        json!({"error":format!("{:#}",failure.cause),"stop_timing":failure.timing})
    } else {
        json!({"error":format!("{error:#}")})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn refusal_keeps_wording_and_fixed_bounded_timing_fields() {
        let timing = Timing::new();
        let cause = anyhow::anyhow!("Mode transition deadline reached; retain its original UUID.");
        let detail = error_detail(&timing.failure(cause));
        assert_eq!(
            detail["error"],
            "Mode transition deadline reached; retain its original UUID."
        );
        let mut snapshot = detail["stop_timing"].clone();
        assert!(snapshot["elapsed_ms"].as_u64().unwrap() <= 60_000);
        assert!(snapshot["unattributed_ms"].as_u64().unwrap() <= 60_000);
        snapshot["elapsed_ms"] = 0.into();
        snapshot["unattributed_ms"] = 0.into();
        assert_eq!(
            snapshot,
            json!({"abi":"desktop-mode-stop-timing-v1","budget_ms":2000,
            "elapsed_ms":0,"unattributed_ms":0,"steps":{
                "native_rechecks":{"calls":0,"elapsed_ms":0},
                "metadata_sync":{"calls":0,"elapsed_ms":0},
                "instance_stop":{"calls":0,"elapsed_ms":0},
                "physical_exit":{"calls":0,"elapsed_ms":0}}})
        );
        assert!(serde_json::to_vec(&detail).unwrap().len() < 1024);
        assert_eq!(
            error_detail(&anyhow::anyhow!("unchanged preflight")),
            json!({"error":"unchanged preflight"})
        );
    }

    // The wrapper's Display prints the cause; the error chain must not print
    // it a second time (G1 review of PR #2140).
    #[test]
    fn failure_alternate_format_matches_a_cause_without_a_source() {
        let timing = Timing::new();
        let text = "Mode transition deadline reached; retain its original UUID.";
        let plain = anyhow::anyhow!(text);
        let wrapped = timing.failure(anyhow::anyhow!(text));
        assert_eq!(format!("{:#}", wrapped), format!("{:#}", plain));
    }

    #[test]
    fn failure_alternate_format_keeps_one_inner_source_exactly_once() {
        #[derive(Debug)]
        struct Inner;
        impl fmt::Display for Inner {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("native stop failed")
            }
        }
        impl std::error::Error for Inner {}

        #[derive(Debug)]
        struct Outer(Inner);
        impl fmt::Display for Outer {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("mode transition refused")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let timing = Timing::new();
        let plain = anyhow::Error::from(Outer(Inner));
        let wrapped = timing.failure(anyhow::Error::from(Outer(Inner)));
        let formatted = format!("{:#}", wrapped);
        assert_eq!(formatted, format!("{:#}", plain));
        assert_eq!(formatted.matches("native stop failed").count(), 1);
        assert_eq!(formatted.matches("mode transition refused").count(), 1);
    }

    #[test]
    fn nested_rechecks_are_exclusive_and_elapsed_values_are_capped() {
        let timing = Timing::new();
        let base = timing.start;
        timing.transition(Some(Step::MetadataSync), base);
        timing.transition(Some(Step::NativeRechecks), base + Duration::from_millis(10));
        timing.transition(Some(Step::MetadataSync), base + Duration::from_millis(30));
        timing.transition(None, base + Duration::from_millis(40));
        let snapshot = timing.snapshot_at(base + Duration::from_millis(50));
        assert_eq!(snapshot["steps"]["metadata_sync"]["elapsed_ms"], 20);
        assert_eq!(snapshot["steps"]["native_rechecks"]["elapsed_ms"], 20);
        assert_eq!(snapshot["unattributed_ms"], 10);
        timing.transition(Some(Step::PhysicalExit), base + Duration::from_millis(50));
        timing.transition(None, base + Duration::from_secs(100_000));
        let snapshot = timing.snapshot_at(base + Duration::from_secs(100_000));
        assert_eq!(snapshot["elapsed_ms"], 60_000);
        assert_eq!(snapshot["steps"]["physical_exit"]["elapsed_ms"], 60_000);
    }

    #[test]
    fn failed_measured_operation_restores_the_parent_step() {
        let timing = Timing::new();
        let result: anyhow::Result<()> = timing.measure(Step::MetadataSync, || {
            timing.measure(Step::NativeRechecks, || anyhow::bail!("same error"))
        });
        assert_eq!(result.unwrap_err().to_string(), "same error");
        assert!(timing.active.get().is_none());
        let snapshot = timing.snapshot();
        assert_eq!(snapshot["steps"]["metadata_sync"]["calls"], 1);
        assert_eq!(snapshot["steps"]["native_rechecks"]["calls"], 1);
    }
}
