//! Dormant Cocoa observer ownership. No production caller or role authority.
//! Removal stops registration; only final native block destruction ends its
//! exact Observed capture. The host must also wait on its own Attention gate.
#![allow(dead_code)]
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{
    NSApplicationDidBecomeActiveNotification, NSApplicationDidResignActiveNotification,
};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSString};
use zerus_client_core::desktop::drain::{Charge, Closed, Role};

use crate::{
    desk::observers::{Control, Observers, Refused},
    notifier::accounting::Completion,
};

type Action = Box<dyn Fn() + Send + Sync>;
type Callback = RcBlock<dyn Fn(NonNull<NSNotification>)>;
struct Entry {
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
    block: Callback,
}

pub(crate) struct ActivityObservers {
    entries: [Option<Entry>; 2],
    center: Retained<NSNotificationCenter>,
    // Declared before the factory: unconfirmed owner destruction pins the same
    // existing Completion group, never a new registry or independent charge.
    owner: Option<Completion>,
    factory: Observers,
    control: Control,
}
impl ActivityObservers {
    /// Optional only. Actions are synchronous and must not queue unaccounted
    /// work. The existing follow_activity/default GUI does not call this.
    pub fn install(charge: Charge, active: Action, inactive: Action) -> Result<Self, Refused> {
        if charge.role() != Role::Attention {
            return Err(Refused::WrongRole);
        }
        // SAFETY: AppKit's immutable constant names; no owner app is queried.
        let names = unsafe {
            [
                NSApplicationDidBecomeActiveNotification,
                NSApplicationDidResignActiveNotification,
            ]
        };
        Self::at(
            NSNotificationCenter::defaultCenter(),
            names,
            charge,
            [active, inactive],
        )
    }

    fn at(
        center: Retained<NSNotificationCenter>,
        names: [&NSString; 2],
        charge: Charge,
        actions: [Action; 2],
    ) -> Result<Self, Refused> {
        let (factory, control) = Observers::new(charge)?;
        let owner = factory.retain_owner()?;
        let mut adapter = Self {
            entries: [None, None],
            center,
            owner: Some(owner),
            factory,
            control,
        };
        for (index, (name, action)) in names.into_iter().zip(actions).enumerate() {
            let observed = adapter.factory.wrap(action).map_err(|(error, _)| error)?;
            // Move the WHOLE Observed into the actual native block. Cocoa's
            // copies retain this exact capture, not a newly charged wrapper.
            let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
                let _ = observed.call();
            });
            // SAFETY: no object filter or operation queue. The closure captures
            // only the Send + Sync Observed and runs synchronously on posting.
            let token = unsafe {
                adapter.center.addObserverForName_object_queue_usingBlock(
                    Some(name),
                    None,
                    None,
                    &block,
                )
            };
            adapter.entries[index] = Some(Entry { token, block });
        }
        Ok(adapter)
    }

    /// Fence first, then remove exactly the original registrations and release
    /// our token/block references. Copies and callbacks may still keep Pending.
    /// This closed capture gate is not a complete Attention drain or role proof.
    pub fn close(&mut self) -> Result<Closed, Refused> {
        let closed = self.control.close()?;
        self.remove();
        if let Some(owner) = self.owner.take() {
            owner.complete();
        }
        Ok(closed)
    }

    fn remove(&mut self) {
        for entry in &mut self.entries {
            if let Some(original) = entry.take() {
                // SAFETY: this token came from this exact center. No name,
                // process-wide removal, replacement or reconstructed identity.
                unsafe { self.center.removeObserver(original.token.as_ref()) };
                drop(original);
            }
        }
    }
}
impl Drop for ActivityObservers {
    fn drop(&mut self) {
        // Abandonment fences late calls and removes known registrations, but
        // deliberately DOES NOT complete owner. Its existing Drop pins even
        // if every SDK capture happens to disappear during this cleanup.
        let _ = self.control.close();
        self.remove();
    }
}
impl std::fmt::Debug for ActivityObservers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ActivityObservers([owned native captures])")
    }
}

#[cfg(test)]
mod tests;
