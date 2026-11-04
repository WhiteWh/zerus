//! Read-only installation preflight and original in-process WinRT objects.
//! Registration is an explicit installer operation; this worker never repairs it.
use super::{
    activation,
    callbacks::{Candidate, Slot},
    wire, Native,
};
use crate::{config::Config, desktop::store::Location, host, platform};
use ::windows::{
    core::{IInspectable, HSTRING},
    Data::Xml::Dom::XmlDocument,
    Foundation::TypedEventHandler,
    Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
    UI::Notifications::{
        NotificationSetting, ToastDismissalReason, ToastDismissedEventArgs, ToastFailedEventArgs,
        ToastNotification, ToastNotificationManager, ToastNotifier,
    },
};
use std::{collections::BTreeMap, mem::size_of, ptr};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::{
        Registry::*, RemoteDesktop::*, StationsAndDesktops::*, Threading::GetCurrentThreadId,
    },
};
use wire::{Action, Command, NativeId, Platform, Refusal, Session};

const APPLICATION_KEY: &str = r"Software\Classes\AppUserModelId\com.hgdev.hgs-tray";
const ABI_VALUE: &str = "HGSDesktopNotificationABI";
const CONFIGURATION_VALUE: &str = "HGSDesktopConfiguration";

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}
fn registry_string(key: &str, name: &str) -> Option<String> {
    let mut value = vec![0u16; 32768];
    let mut bytes = (value.len() * size_of::<u16>()) as u32;
    // A fixed local HKCU value, one bounded read, no expansion or fallback view.
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(key).as_ptr(),
            wide(name).as_ptr(),
            RRF_RT_REG_SZ | RRF_NOEXPAND,
            ptr::null_mut(),
            value.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if result != 0 || bytes < 2 || bytes as usize > value.len() * 2 || !bytes.is_multiple_of(2) {
        return None;
    }
    value.truncate(bytes as usize / 2);
    if value.pop() != Some(0) || value.contains(&0) {
        return None;
    }
    String::from_utf16(&value).ok()
}

fn object_name(handle: HANDLE) -> Option<String> {
    let mut name = [0u16; 128];
    let mut bytes = 0;
    if handle.is_null()
        || unsafe {
            GetUserObjectInformationW(handle, UOI_NAME, name.as_mut_ptr().cast(), 256, &mut bytes)
        } == 0
        || !(2..=256).contains(&bytes)
        || !bytes.is_multiple_of(2)
    {
        return None;
    }
    let name = &name[..bytes as usize / 2];
    if name.last() != Some(&0) {
        return None;
    }
    String::from_utf16(&name[..name.len() - 1]).ok()
}
fn interactive_session() -> Option<u32> {
    let id = host::token::windows_session().ok()?;
    if id == 0 {
        return None;
    }
    let (mut data, mut bytes) = (ptr::null_mut(), 0);
    if unsafe {
        WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            id,
            WTSConnectState,
            &mut data,
            &mut bytes,
        )
    } == 0
    {
        return None;
    }
    let state = if !data.is_null() && bytes as usize == size_of::<WTS_CONNECTSTATE_CLASS>() {
        Some(unsafe { ptr::read_unaligned(data.cast::<WTS_CONNECTSTATE_CLASS>()) })
    } else {
        None
    };
    if !data.is_null() {
        unsafe { WTSFreeMemory(data.cast()) };
    }
    if state != Some(WTSActive)
        || object_name(unsafe { GetProcessWindowStation() })?.as_str() != "WinSta0"
        || object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })?.as_str() != "Default"
    {
        return None;
    }
    Some(id)
}

struct Installation {
    configuration: String,
    sid: String,
    session: u32,
    command: String,
}
impl Installation {
    fn select(config: &Config, session: &Session) -> Result<Self, &'static str> {
        let location = Location::new(config).map_err(|_| "configuration_unavailable")?;
        if !session.valid()
            || session.platform != Platform::Windows
            || location.configuration != session.configuration
        {
            return Err("configuration_changed");
        }
        // This maintained plan derives the actual token SID and selected stable
        // CLI; no incoming JSON plan or role assertion is accepted here.
        let plan = crate::desktop::registration::windows_plan(config, &location)
            .map_err(|_| "selected_cli_unavailable")?;
        let cli = plan["argv"][0].as_str().ok_or("selected_cli_unavailable")?;
        let here =
            platform::canonical(&std::env::current_exe().map_err(|_| "selected_cli_unavailable")?)
                .map_err(|_| "selected_cli_unavailable")?;
        if platform::canonical(std::path::Path::new(cli)).ok().as_ref() != Some(&here)
            || cli.contains('"')
        {
            return Err("selected_cli_changed");
        }
        let args = [
            activation::ENTRY,
            "--config-dir",
            plan["argv"][4]
                .as_str()
                .ok_or("configuration_unavailable")?,
            "--state-dir",
            plan["argv"][6]
                .as_str()
                .ok_or("configuration_unavailable")?,
        ];
        let command = format!(
            "\"{cli}\" {}",
            args.iter()
                .map(|v| host::spawn::quote_argument(v))
                .collect::<Vec<_>>()
                .join(" ")
        );
        if command.encode_utf16().count() >= 32767 {
            return Err("registration_command_too_large");
        }
        let sid = host::token::user_sid().map_err(|_| "user_unavailable")?;
        if matches!(sid.as_str(), "S-1-5-18" | "S-1-5-19" | "S-1-5-20")
            || sid.starts_with("S-1-5-80-")
        {
            return Err("interactive_user_required");
        }
        Ok(Self {
            configuration: location.configuration,
            sid,
            session: interactive_session().ok_or("interactive_session_unavailable")?,
            command,
        })
    }
    fn check(&self) -> bool {
        let class = format!(r"Software\Classes\CLSID\{}", activation::CLSID_TEXT);
        host::token::user_sid().ok().as_ref() == Some(&self.sid)
            && interactive_session() == Some(self.session)
            && registry_string(APPLICATION_KEY, ABI_VALUE).as_deref() == Some(wire::ABI)
            && registry_string(APPLICATION_KEY, CONFIGURATION_VALUE).as_ref()
                == Some(&self.configuration)
            && registry_string(APPLICATION_KEY, "CustomActivator").as_deref()
                == Some(activation::CLSID_TEXT)
            && registry_string(APPLICATION_KEY, "DisplayName").as_deref() == Some("Zerus")
            && registry_string(&class, ABI_VALUE).as_deref() == Some(wire::ABI)
            && registry_string(&class, CONFIGURATION_VALUE).as_ref() == Some(&self.configuration)
            && registry_string(&format!(r"{class}\LocalServer32"), "").as_ref()
                == Some(&self.command)
    }
}

struct Observed {
    toast: ToastNotification,
    activated: Option<i64>,
    dismissed: Option<i64>,
    failed: Option<i64>,
    retired: bool,
}
impl Observed {
    fn detach(&mut self) -> bool {
        if self
            .activated
            .is_some_and(|id| self.toast.RemoveActivated(id).is_ok())
        {
            self.activated = None;
        }
        if self
            .dismissed
            .is_some_and(|id| self.toast.RemoveDismissed(id).is_ok())
        {
            self.dismissed = None;
        }
        if self
            .failed
            .is_some_and(|id| self.toast.RemoveFailed(id).is_ok())
        {
            self.failed = None;
        }
        self.activated.is_none() && self.dismissed.is_none() && self.failed.is_none()
    }
}

pub(super) struct Backend {
    installation: Installation,
    notifier: ToastNotifier,
    slot: Slot,
    registration: activation::Registration,
    observed: BTreeMap<String, Observed>,
    apartment: bool,
    unavailable: bool,
}
impl Backend {
    pub fn open(config: &Config, session: &Session, slot: Slot) -> Result<Self, &'static str> {
        let installation = Installation::select(config, session)?;
        if !installation.check() {
            return Err("notification_registration_unavailable");
        }
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map_err(|_| "notification_apartment_unavailable")?;
        let initialized = (|| {
            let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
                wire::APPLICATION,
            ))
            .map_err(|_| "notification_notifier_unavailable")?;
            let registration = activation::Registration::register(slot.clone())
                .map_err(|_| "notification_activator_unavailable")?;
            Ok(Self {
                installation,
                notifier,
                slot,
                registration,
                observed: BTreeMap::new(),
                apartment: true,
                unavailable: false,
            })
        })();
        if initialized.is_err() {
            unsafe { RoUninitialize() };
        }
        initialized
    }
    fn shutdown(&mut self) -> bool {
        self.slot.request_stop();
        if self.registration.revoke().is_err() {
            return false;
        }
        self.observed.retain(|_, value| !value.detach());
        if !self.observed.is_empty() || self.slot.retained() != 0 {
            return false;
        }
        if self.apartment {
            unsafe { RoUninitialize() };
            self.apartment = false;
        }
        true
    }
    fn history_collision(&self, id: &NativeId) -> Result<bool, ()> {
        let NativeId::Windows { tag, group } = id else {
            return Err(());
        };
        let values = ToastNotificationManager::History()
            .map_err(|_| ())?
            .GetHistoryWithId(&HSTRING::from(wire::APPLICATION))
            .map_err(|_| ())?;
        let count = values.Size().map_err(|_| ())?;
        if count as usize > wire::MAPPINGS {
            return Err(());
        }
        for i in 0..count {
            let value = values.GetAt(i).map_err(|_| ())?;
            if value.Tag().map_err(|_| ())? == tag.as_str()
                && value.Group().map_err(|_| ())? == group.as_str()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
impl Native for Backend {
    fn unavailable(&self) -> bool {
        self.unavailable
    }
    fn callbacks_current(&mut self) -> bool {
        if !self.installation.check() {
            self.unavailable = true;
        }
        !self.unavailable
    }
    fn close(&mut self) -> bool {
        self.shutdown()
    }
    fn retire(&mut self, request_id: &str) {
        if let Some(value) = self.observed.get_mut(request_id) {
            value.retired = true;
        }
        self.observed
            .retain(|_, value| !value.retired || !value.detach());
    }
    fn prepare(&mut self, command: &Command, id: &NativeId) -> Result<(), Refusal> {
        if !self.apartment || !self.callbacks_current() {
            return Err(Refusal::NotReady);
        }
        let Action::Post { title, body, .. } = &command.action else {
            // Withdrawing an original shown toast is cleanup, even if posting
            // permission was disabled after its original Show.
            return Ok(());
        };
        if self.notifier.Setting().ok() != Some(NotificationSetting::Enabled) {
            return Err(Refusal::PermissionDenied);
        }
        self.observed
            .retain(|_, value| !value.retired || !value.detach());
        if self.observed.len() >= wire::MAPPINGS {
            return Err(Refusal::Capacity);
        }
        if self.history_collision(id).map_err(|_| Refusal::NotReady)? {
            return Err(Refusal::NotReady);
        }
        let NativeId::Windows { tag, group } = id else {
            return Err(Refusal::InvalidCommand);
        };
        if self.observed.contains_key(&command.binding.request_id) {
            return Err(Refusal::InvalidCommand);
        }
        let xml = XmlDocument::new().map_err(|_| Refusal::Unsupported)?;
        let escape = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;")
                .replace('\'', "&apos;")
        };
        let document=format!("<toast launch=\"{}{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",super::callbacks::PREFIX,command.binding.request_id,escape(title),escape(body));
        xml.LoadXml(&HSTRING::from(document))
            .map_err(|_| Refusal::InvalidCommand)?;
        let toast =
            ToastNotification::CreateToastNotification(&xml).map_err(|_| Refusal::Unsupported)?;
        toast
            .SetTag(&HSTRING::from(tag))
            .and_then(|()| toast.SetGroup(&HSTRING::from(group)))
            .map_err(|_| Refusal::InvalidCommand)?;
        let request = command.binding.request_id.clone();
        self.observed.insert(
            request.clone(),
            Observed {
                toast,
                activated: None,
                dismissed: None,
                failed: None,
                retired: false,
            },
        );
        let value = self
            .observed
            .get_mut(&request)
            .expect("original prepared toast");
        let sink = self.slot.clone();
        let id = request.clone();
        let held = self.slot.hold().ok_or(Refusal::Capacity)?;
        value.activated = Some(
            value
                .toast
                .Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
                    move |_, _| {
                        let _held = &held;
                        sink.send(Candidate::Clicked(id.clone()));
                        Ok(())
                    },
                ))
                .map_err(|_| Refusal::Unsupported)?,
        );
        let sink = self.slot.clone();
        let id = request.clone();
        let held = self.slot.hold().ok_or(Refusal::Capacity)?;
        value.dismissed = Some(
            value
                .toast
                .Dismissed(&TypedEventHandler::<
                    ToastNotification,
                    ToastDismissedEventArgs,
                >::new(move |_, args| {
                    let _held = &held;
                    // A timed-out banner still lives in Action Center. Never turn its
                    // disappearance into a withdrawal or discard its click mapping.
                    if args.as_ref().and_then(|v| v.Reason().ok())
                        == Some(ToastDismissalReason::UserCanceled)
                    {
                        sink.send(Candidate::Closed(id.clone()));
                    }
                    Ok(())
                }))
                .map_err(|_| Refusal::Unsupported)?,
        );
        let sink = self.slot.clone();
        let held = self.slot.hold().ok_or(Refusal::Capacity)?;
        value.failed = Some(
            value
                .toast
                .Failed(
                    &TypedEventHandler::<ToastNotification, ToastFailedEventArgs>::new(
                        move |_, _| {
                            let _held = &held;
                            sink.send(Candidate::Closed(request.clone()));
                            Ok(())
                        },
                    ),
                )
                .map_err(|_| Refusal::Unsupported)?,
        );
        Ok(())
    }
    fn post(&mut self, command: &Command, _: &NativeId) -> Result<NativeId, ()> {
        let value = self.observed.get(&command.binding.request_id).ok_or(())?;
        self.notifier.Show(&value.toast).map_err(|_| ())?;
        Ok(NativeId::Windows {
            tag: value.toast.Tag().map_err(|_| ())?.to_string(),
            group: value.toast.Group().map_err(|_| ())?.to_string(),
        })
    }
    fn withdraw(&mut self, id: &NativeId) -> Result<(), ()> {
        let NativeId::Windows { tag, group } = id else {
            return Err(());
        };
        ToastNotificationManager::History()
            .map_err(|_| ())?
            .RemoveGroupedTagWithId(
                &HSTRING::from(tag),
                &HSTRING::from(group),
                &HSTRING::from(wire::APPLICATION),
            )
            .map_err(|_| ())
    }
}
