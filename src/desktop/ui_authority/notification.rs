//! A native focus observation is a bounded hint, never a mutation permit.
use super::*;
use zerus_client_core::desktop::notification_control::{Command, NativeFocus, Scope, FOCUS_TTL_MS};

pub(in crate::desktop) struct FocusPeer {
    freshness: Freshness,
    until: Instant,
    #[cfg(windows)]
    next_full: Cell<Instant>,
}
impl FocusPeer {
    #[cfg(test)]
    pub(in crate::desktop) fn expire_for_test(&mut self) {
        self.until = Instant::now();
    }
    pub(in crate::desktop) fn until(&self) -> Instant {
        self.until
    }
    pub(in crate::desktop) fn current(&self) -> bool {
        let now = Instant::now();
        if now >= self.until {
            return false;
        }
        match &self.freshness {
            Freshness::Native {
                frontend,
                #[cfg(windows)]
                windows,
                ..
            } => {
                // The original relay is expected to exit after its response.
                // Only the retained canonical frontend keeps this hint live.
                if !frontend.current() {
                    return false;
                }
                #[cfg(windows)]
                {
                    let Some(proof) = windows else {
                        return false;
                    };
                    if !proof.canonical.current() {
                        return false;
                    }
                    if now >= self.next_full.get() {
                        let until = self.until.min(now + WAIT);
                        let Some(original) = &frontend.original else {
                            return false;
                        };
                        if proof.ancestry.validate_parent(original, until).is_err()
                            || !proof.canonical.current()
                            || Instant::now() >= until
                        {
                            return false;
                        }
                        self.next_full.set(now + ACTIVATION_PROOF_INTERVAL);
                    }
                }
                true
            }
            #[cfg(test)]
            Freshness::Fixture => true,
        }
    }
}
impl Validated {
    pub(in crate::desktop) fn notification_parts(&self) -> Result<(Scope, Command)> {
        self.check()?;
        let Request::NotificationControl { scope, control } = &self.0 else {
            anyhow::bail!("not notification controls")
        };
        let native = self
            .2
            .notification
            .as_ref()
            .context("original native notification evidence missing")?;
        ensure!(
            scope.valid()
                && control.valid()
                && native.scope == *scope
                && native.mode == zerus_client_core::desktop::ownership::Mode::Service,
            "native notification scope changed"
        );
        Ok((scope.clone(), control.clone()))
    }
    pub(in crate::desktop) fn notification_focus(
        self,
    ) -> Result<(Scope, String, NativeFocus, FocusPeer)> {
        let (scope, control) = self.notification_parts()?;
        let Command::Focus { request_id } = control else {
            anyhow::bail!("not focus")
        };
        let focus = self
            .2
            .notification
            .context("native notification evidence missing")?
            .focus
            .context("native focus missing")?;
        ensure!(focus.valid(), "invalid native focus");
        let now = Instant::now();
        Ok((
            scope,
            request_id,
            focus,
            FocusPeer {
                freshness: self.1,
                until: now + Duration::from_millis(FOCUS_TTL_MS),
                #[cfg(windows)]
                next_full: Cell::new(now + ACTIVATION_PROOF_INTERVAL),
            },
        ))
    }
    #[cfg(test)]
    pub(in crate::desktop) fn notification_fixture(
        scope: Scope,
        control: Command,
        focus: Option<NativeFocus>,
    ) -> Self {
        Self(
            Request::NotificationControl {
                scope: scope.clone(),
                control,
            },
            Freshness::Fixture,
            Metadata {
                ownership: None,
                notification: Some(
                    zerus_client_core::desktop::notification_control::NativeControl {
                        scope,
                        mode: zerus_client_core::desktop::ownership::Mode::Service,
                        focus,
                    },
                ),
            },
        )
    }
}
