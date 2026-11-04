//! Linux launch mechanics only. The actor supplies its original absence-bound
//! Launch and explicit installed selection; this module makes neither decision.
use super::{ChildObservation, SelectedFrontend};
use crate::{desktop::notifications::activation::Launch, platform};
use anyhow::{ensure, Context, Result};
use std::{
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use zerus_client_core::{
    actions::Invocation,
    desktop::{activation as wire, notifications::PlatformContext},
};

const START: Duration = Duration::from_secs(10);
const CHECK: Duration = Duration::from_millis(250);
static ACTIVE: AtomicBool = AtomicBool::new(false);
struct Admission;
impl Drop for Admission {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}
struct Hold {
    selected: SelectedFrontend,
    _admission: Admission,
}

/// Original Child, selection and click retained through wrapper exec and exit.
/// There are no output pipes: GUI output is sent to /dev/null. This is NOT a
/// readiness/exit receipt or permission to replay a failed/uncertain launch.
#[must_use = "retain and poll the original frontend child until its actual exit"]
pub(in crate::desktop) struct OwnedFrontend {
    hold: Arc<Hold>,
    launch: Option<Launch>,
    child: Option<Child>,
    observed: Option<ChildObservation>,
    until: Instant,
    next_check: Instant,
    error: bool,
    exit: Option<ExitStatus>,
}
impl OwnedFrontend {
    /// Synchronous bounded preparation: run OFF the service actor. One original
    /// launch admission includes its help child and any incomplete pipe drain.
    /// An error consumes this Launch; it never authorizes a retry.
    pub fn launch(selected: SelectedFrontend, mut launch: Launch) -> Result<Self> {
        let until = Instant::now() + START;
        selected.revalidate_for_launch(&launch)?;
        ensure!(launch.open().valid(), "invalid original launch");
        ensure!(
            ACTIVE
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok(),
            "frontend launch still owned"
        );
        let hold = Arc::new(Hold {
            selected,
            _admission: Admission,
        });
        let proof = hold.clone();
        let invocation = Invocation {
            args: vec!["--help".into()],
            request_id: None,
            stdin: Some(Vec::new()),
            timeout: until.saturating_duration_since(Instant::now()),
        };
        // The actual Root owner survives an uncertain help-child/pipe drain,
        // but is not charged to the GUI's potentially unbounded lifetime.
        let resources = Arc::new((hold.clone(), launch.preparation_resources()));
        let help = zerus_client_core::run::run_owned(
            hold.selected.entry(),
            &invocation,
            &[],
            &AtomicBool::new(false),
            move |command| {
                proof.selected.revalidate().map_err(std::io::Error::other)?;
                command
                    .env_remove("XDG_ACTIVATION_TOKEN")
                    .env_remove(wire::NAVIGATION_ENV);
                platform::spawn_prepared_in_turn_before(until, || {
                    proof.selected.revalidate().map_err(std::io::Error::other)?;
                    Ok((std::mem::replace(command, Command::new("")), ()))
                })
                .map(|(child, ())| child)
            },
            resources,
        )
        .map_err(|_| anyhow::anyhow!("frontend navigation capability unavailable"))?;
        ensure!(help.len() <= 64 * 1024, "frontend help too large");
        let help = std::str::from_utf8(&help).context("invalid frontend help")?;
        ensure!(
            help.lines()
                .any(|line| line == wire::NAVIGATION_ENV_HELP_MARKER),
            "frontend target navigation unsupported"
        );
        hold.selected.revalidate_for_launch(&launch)?;
        let command = command(&hold.selected, &launch)?;
        let (child, ()) = platform::spawn_prepared_in_turn_before(until, || {
            hold.selected
                .revalidate_for_launch(&launch)
                .map_err(std::io::Error::other)?;
            Ok((command, ()))
        })?;
        launch.release_preparation();
        // No fallible construction after spawn may drop the original Child.
        Ok(Self {
            hold,
            launch: Some(launch),
            child: Some(child),
            observed: None,
            until,
            next_check: Instant::now(),
            error: false,
            exit: None,
        })
    }
    /// Call off actor. Expensive selection/NSS checks run at most every 250 ms,
    /// including while a maintained wrapper has not yet execed the held image.
    /// Failure retains the original child for actual exit polling; never adopt
    /// another PID, kill the GUI on service stop, or call image observation ready.
    pub fn poll(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.exit = Some(status);
                self.child.take();
                return;
            }
            Err(_) => {
                self.error = true;
                return;
            }
            Ok(None) => (),
        }
        let now = Instant::now();
        if self.error || now < self.next_check {
            return;
        }
        self.next_check = now + CHECK;
        if let Some(observed) = self.observed.as_ref() {
            self.error = self
                .hold
                .selected
                .revalidate_child(child, observed)
                .is_err();
        } else if now >= self.until {
            self.error = true;
        } else {
            match self.hold.selected.observe_original_child(child) {
                Ok(value) => self.observed = value,
                Err(_) => self.error = true,
            }
        }
    }
    pub fn image_observed(&self) -> bool {
        self.observed.is_some() && !self.error && self.exit.is_none()
    }
    pub fn error(&self) -> bool {
        self.error
    }
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit
    }
    pub fn exited(&self) -> bool {
        self.exit.is_some()
    }
}
impl Drop for OwnedFrontend {
    fn drop(&mut self) {
        // The GUI may outlive the service. Dropping a live/unknown handle does
        // not terminate it or claim that its admission/resources drained. One
        // static admission remains pinned until this service process exits.
        if let Some(child) = self.child.take() {
            std::mem::forget((child, self.hold.clone(), self.launch.take()));
        }
    }
}
fn command(selected: &SelectedFrontend, launch: &Launch) -> Result<Command> {
    let target =
        wire::encode_navigation(&launch.open().target).context("invalid desktop target")?;
    let mut command = Command::new(selected.entry());
    command
        .arg("--hgs")
        .arg(selected.service_image())
        .arg("--sessions")
        .env(wire::NAVIGATION_ENV, target)
        .env_remove("HGS_SESSION")
        .env_remove("HGS_RUN_ID")
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env("ZERUS_CLIENT_DESKTOP_SERVICE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(PlatformContext::Wayland { activation_token }) = &launch.open().platform_context {
        command.env("XDG_ACTIVATION_TOKEN", activation_token);
    }
    Ok(command)
}

#[cfg(test)]
mod tests;
