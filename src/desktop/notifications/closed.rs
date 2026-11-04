//! Linux closed-instance selection and original launch custody, off the actor.
//! No status/caption/PATH inference can manufacture the native ownership grant.
use super::{
    activation,
    frontend::{linux::OwnedFrontend, SelectedFrontend},
    preparation::Owner,
    Outbox,
};
use crate::{
    config::Config,
    desktop::{
        owner,
        ownership::Grant,
        store::{Location, Store},
        ui_authority::Scope,
    },
};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use zerus_client_core::desktop;

struct Selected {
    image: SelectedFrontend,
    observation: desktop::Observation,
    grant: Grant,
}
pub(in crate::desktop) struct Runtime {
    scope: Arc<Scope>,
    selecting: Option<JoinHandle<Result<Selected>>>,
    launching: Option<JoinHandle<Result<()>>>,
    observing: Arc<AtomicBool>,
    observer_detached: bool,
    stop: Arc<AtomicBool>,
    next: Instant,
    offered: u64,
    error: Option<String>,
}
impl Runtime {
    pub fn new(config: &Config, location: &Location) -> Self {
        Self {
            scope: Arc::new(Scope::new(config, location)),
            selecting: None,
            launching: None,
            observing: Arc::new(AtomicBool::new(false)),
            observer_detached: false,
            stop: Arc::new(AtomicBool::new(false)),
            next: Instant::now(),
            offered: 0,
            error: None,
        }
    }
    pub fn poll(
        &mut self,
        store: &Store,
        grant: &Grant,
        owner: &Arc<Owner>,
        outbox: &Outbox,
        router: &mut activation::Router,
    ) {
        if self.selecting.as_ref().is_some_and(|w| w.is_finished()) {
            let result = self
                .selecting
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("frontend selector panicked")));
            if !self.stop.load(Ordering::Acquire) {
                let result = result.and_then(|selected| {
                    ensure!(selected.grant.current(store)?, "selected grant changed");
                    let Some(mut launch) = router.closed_frontend(
                        store,
                        outbox,
                        &selected.observation,
                        Some(&selected.grant),
                    )?
                    else {
                        return Ok(());
                    };
                    launch.bind_preparation(owner.clone(), self.stop.clone());
                    let observing = self.observing.clone();
                    // From here the original click is consumed. Any failure or
                    // uncertain spawn is not permission to launch it again.
                    self.offered = self.offered.saturating_add(1);
                    self.launching = Some(
                        thread::Builder::new()
                            .name("notification-frontend".into())
                            .spawn(move || {
                                observe(OwnedFrontend::launch(selected.image, launch)?, observing);
                                Ok(())
                            })?,
                    );
                    Ok(())
                });
                if let Err(error) = result {
                    self.error = Some(bounded(error));
                }
            }
        }
        if self.launching.as_ref().is_some_and(|w| w.is_finished()) {
            if let Err(error) = self
                .launching
                .take()
                .unwrap()
                .join()
                .unwrap_or_else(|_| Err(anyhow::anyhow!("frontend launcher panicked")))
            {
                self.error = Some(bounded(error));
            }
            self.observing.store(false, Ordering::Release);
        }
        if self.stop.load(Ordering::Acquire)
            || self.selecting.is_some()
            || self.launching.is_some()
            || !router.needs_frontend()
            || Instant::now() < self.next
        {
            return;
        }
        self.next = Instant::now() + Duration::from_secs(1);
        let (scope, grant, owner, stopped) = (
            self.scope.clone(),
            grant.clone(),
            owner.clone(),
            self.stop.clone(),
        );
        let worker = thread::Builder::new()
            .name("notification-frontend-selection".into())
            .spawn(move || {
                let _owner = owner;
                ensure!(
                    !stopped.load(Ordering::Acquire),
                    "frontend selection stopped"
                );
                grant.revalidate_image()?;
                let image = scope.select_frontend(grant.image())?;
                let observation = desktop::probe_at(
                    &owner::instance_path(),
                    &uuid::Uuid::new_v4().to_string(),
                    &scope.configuration,
                );
                image.revalidate()?;
                grant.revalidate_image()?;
                ensure!(
                    !stopped.load(Ordering::Acquire),
                    "frontend selection stopped"
                );
                Ok(Selected {
                    image,
                    observation,
                    grant,
                })
            });
        match worker {
            Ok(worker) => self.selecting = Some(worker),
            Err(error) => self.error = Some(error.to_string()),
        }
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn drained(&mut self) -> bool {
        // Even stopped selectors are joined, never dropped on their timeout.
        if self.selecting.as_ref().is_some_and(|w| w.is_finished()) {
            let _ = self.selecting.take().unwrap().join();
        }
        if self.launching.as_ref().is_some_and(|w| w.is_finished()) {
            let _ = self.launching.take().unwrap().join();
            self.observing.store(false, Ordering::Release);
        }
        if self.stop.load(Ordering::Acquire) && self.observing.load(Ordering::Acquire) {
            // GUI lifetime is independent of native backend ownership. Retain
            // its SAME Child on the pure observer until real exit/reap, so a
            // revoke/rearm does not leak the launch admission until restart.
            // No SDK call, Root exclusion or effect resource remains there.
            self.launching.take();
            self.observer_detached = true;
        }
        self.stop.load(Ordering::Acquire) && self.selecting.is_none() && self.launching.is_none()
    }
    pub fn status(&self) -> Value {
        json!({"selecting":self.selecting.is_some(),"original_worker":self.launching.is_some(),
            "stopping":self.stop.load(Ordering::Acquire),"offered":self.offered,"error":self.error,
            "observing_gui":self.observing.load(Ordering::Acquire),"gui_observer_retained":self.observer_detached,"gui_exit_is_not_required":true})
    }
    #[cfg(test)]
    pub(in crate::desktop) fn observe_for_test(&mut self, frontend: OwnedFrontend) {
        let observing = self.observing.clone();
        self.launching = Some(thread::spawn(move || {
            observe(frontend, observing);
            Ok(())
        }));
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop();
    }
}
fn bounded(error: anyhow::Error) -> String {
    format!("{error:#}").chars().take(256).collect()
}
fn observe(mut frontend: OwnedFrontend, observing: Arc<AtomicBool>) {
    // No real Root owner is retained after spawn, also checked by its Arc count.
    observing.store(true, Ordering::Release);
    // Pure observation outlives a backend revoke: never kill the GUI, lose its
    // original Child, or keep its static admission forever after eventual exit.
    while !frontend.exited() {
        frontend.poll();
        thread::sleep(Duration::from_millis(250));
    }
}
