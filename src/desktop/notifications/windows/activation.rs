//! The existing service's native COM slot. Registering the class does not start
//! a service, change registry state or supply a notification writer lease.
use super::callbacks::{Retained, Slot};
use ::windows::{
    core::{implement, IUnknown, Interface, Ref, GUID, HRESULT, PCWSTR},
    Win32::{
        Foundation::{
            CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_INVALIDARG, E_OUTOFMEMORY,
            E_POINTER,
        },
        System::Com::{
            CoRegisterClassObject, CoRevokeClassObject, IClassFactory, IClassFactory_Impl,
            CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE,
        },
        UI::Notifications::{
            INotificationActivationCallback, INotificationActivationCallback_Impl,
            NOTIFICATION_USER_INPUT_DATA,
        },
    },
};
use std::ffi::c_void;

pub(super) const CLSID: GUID = GUID::from_u128(0x0f0f70a2_1d40_47d8_ac5e_d6a9b789605f);
pub(super) const CLSID_TEXT: &str = super::registration::CLASS_ID;
pub(in crate::desktop) const ENTRY: &str = "--desktop-notification-activation";

/// A cold COM launch has no original Runtime/Endpoint. Lifecycle parsing may
/// call this fixed entry, but it cannot acquire/promote the canonical service.
pub(in crate::desktop) fn cold_entry() -> i32 {
    78
}

pub(super) struct Registration {
    cookie: Option<u32>,
    factory: Option<IClassFactory>,
    slot: Slot,
}
impl Registration {
    /// Called only on the original native worker after its apartment and
    /// interactive-user/installation preflight. The slot starts dormant.
    pub fn register(slot: Slot) -> ::windows::core::Result<Self> {
        let factory: IClassFactory = Factory {
            retained: slot
                .hold()
                .ok_or_else(|| ::windows::core::Error::from(E_OUTOFMEMORY))?,
            slot: slot.clone(),
        }
        .into();
        let cookie = unsafe {
            CoRegisterClassObject(&CLSID, &factory, CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE)?
        };
        Ok(Self {
            cookie: Some(cookie),
            factory: Some(factory),
            slot,
        })
    }
    pub fn revoke(&mut self) -> ::windows::core::Result<()> {
        self.slot.request_stop();
        if let Some(cookie) = self.cookie {
            unsafe { CoRevokeClassObject(cookie)? };
            self.cookie = None;
        }
        self.factory.take();
        Ok(())
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        // No drained claim follows from Drop, including a failed revocation.
        let _ = self.revoke();
    }
}

#[implement(IClassFactory)]
struct Factory {
    slot: Slot,
    retained: Retained,
}
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<'_, IUnknown>,
        iid: *const GUID,
        result: *mut *mut c_void,
    ) -> ::windows::core::Result<()> {
        if iid.is_null() || result.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *result = std::ptr::null_mut() };
        if !outer.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        if !self.slot.accepting_activations() {
            return Err(CLASS_E_CLASSNOTAVAILABLE.into());
        }
        let callback: INotificationActivationCallback = Callback {
            slot: self.slot.clone(),
            retained: self
                .slot
                .hold()
                .ok_or_else(|| ::windows::core::Error::from(E_OUTOFMEMORY))?,
        }
        .into();
        // QueryInterface supplies only this original callback object. Arguments
        // received later remain untrusted correlation, never writer authority.
        unsafe { callback.query(iid, result).ok() }
    }
    fn LockServer(&self, _: ::windows::core::BOOL) -> ::windows::core::Result<()> {
        // The Runtime's original owner controls lifetime, not a COM client.
        Ok(())
    }
}

#[implement(INotificationActivationCallback)]
struct Callback {
    slot: Slot,
    retained: Retained,
}
impl INotificationActivationCallback_Impl for Callback_Impl {
    fn Activate(
        &self,
        application: &PCWSTR,
        arguments: &PCWSTR,
        _: *const NOTIFICATION_USER_INPUT_DATA,
        input_count: u32,
    ) -> ::windows::core::Result<()> {
        if input_count != 0 {
            return Err(E_INVALIDARG.into());
        }
        // COM marshals valid strings. Still bound every read/UTF-16 allocation
        // and reject invalid scalars rather than using replacement decoding.
        let application = unsafe { bounded(*application, 64)? };
        let arguments = unsafe { bounded(*arguments, 64)? };
        if self.slot.activate(&application, &arguments, input_count) {
            Ok(())
        } else {
            Err(E_INVALIDARG.into())
        }
    }
}

unsafe fn bounded(value: PCWSTR, cap: usize) -> ::windows::core::Result<String> {
    if value.is_null() {
        return Err(E_POINTER.into());
    }
    for length in 0..=cap {
        if unsafe { *value.0.add(length) } == 0 {
            return String::from_utf16(unsafe { std::slice::from_raw_parts(value.0, length) })
                .map_err(|_| HRESULT(0x80070057u32 as i32).into());
        }
    }
    Err(E_INVALIDARG.into())
}
