//! Fixed HKCU application keys only; no registry discovery, service Run or repair.
use super::*;
use crate::{
    config::Config,
    desktop::{registration, store::Location},
    host, platform,
};
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    os::windows::io::{FromRawHandle, OwnedHandle},
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FILETIME, WAIT_ABANDONED,
        WAIT_OBJECT_0,
    },
    System::{
        Registry::*,
        Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
    },
};

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe { RegCloseKey(self.0) };
    }
}
struct Lock {
    handle: OwnedHandle,
    raw: windows_sys::Win32::Foundation::HANDLE,
}
impl Lock {
    fn acquire(sid: &str) -> Result<Self> {
        let raw = unsafe {
            CreateMutexW(
                ptr::null(),
                0,
                wide(&format!(r"Global\hgs-desktop-toast-registration-{sid}")).as_ptr(),
            )
        };
        ensure!(!raw.is_null(), "notification registration lock unavailable");
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let waited = unsafe { WaitForSingleObject(raw, 2000) };
        if waited != WAIT_OBJECT_0 {
            if waited == WAIT_ABANDONED {
                unsafe { ReleaseMutex(raw) };
            }
            bail!("notification registration lock not confirmed; inspect registration before retrying");
        }
        Ok(Self { handle, raw })
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = &self.handle;
        unsafe { ReleaseMutex(self.raw) };
    }
}
fn open(path: &str) -> Result<Option<Key>, &'static str> {
    let mut key = ptr::null_mut();
    let result = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(path).as_ptr(),
            REG_OPTION_OPEN_LINK,
            KEY_READ | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    match result {
        ERROR_SUCCESS => Ok(Some(Key(key))),
        ERROR_FILE_NOT_FOUND => Ok(None),
        _ => Err("notification_registry_unreadable_kept"),
    }
}
fn stamp(key: HKEY) -> Result<(u32, u32, u32, u32), &'static str> {
    let (mut children, mut values) = (0, 0);
    let mut time = FILETIME::default();
    let result = unsafe {
        RegQueryInfoKeyW(
            key,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
            &mut children,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut values,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut time,
        )
    };
    if result != ERROR_SUCCESS || children > 1 || values > 8 {
        return Err("notification_registry_bounds_refused");
    }
    Ok((children, values, time.dwLowDateTime, time.dwHighDateTime))
}
fn snapshot(key: HKEY) -> Result<Contents, &'static str> {
    let before = stamp(key)?;
    let mut values = BTreeMap::new();
    let mut children = BTreeSet::new();
    for index in 0..=8 {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        let mut data = vec![0u16; 32768];
        let mut bytes = (data.len() * 2) as u32;
        let mut kind = 0;
        let result = unsafe {
            RegEnumValueW(
                key,
                index,
                name.as_mut_ptr(),
                &mut length,
                ptr::null(),
                &mut kind,
                data.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if result == ERROR_NO_MORE_ITEMS {
            break;
        }
        if result != ERROR_SUCCESS
            || index == 8
            || kind != REG_SZ
            || length as usize >= name.len()
            || bytes < 2
            || bytes as usize > data.len() * 2
            || !bytes.is_multiple_of(2)
        {
            return Err("notification_registry_value_refused");
        }
        data.truncate(bytes as usize / 2);
        if data.pop() != Some(0) || data.contains(&0) {
            return Err("notification_registry_string_refused");
        }
        let name = String::from_utf16(&name[..length as usize])
            .map_err(|_| "notification_registry_string_refused")?;
        let value =
            String::from_utf16(&data).map_err(|_| "notification_registry_string_refused")?;
        if values.insert(name, value).is_some() {
            return Err("notification_registry_changed_kept");
        }
    }
    for index in 0..=1 {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        let result = unsafe {
            RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut length,
                ptr::null(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if result == ERROR_NO_MORE_ITEMS {
            break;
        }
        if result != ERROR_SUCCESS || index == 1 || length as usize >= name.len() {
            return Err("notification_registry_child_refused");
        }
        children.insert(
            String::from_utf16(&name[..length as usize])
                .map_err(|_| "notification_registry_string_refused")?,
        );
    }
    if stamp(key)? != before
        || children.len() != before.0 as usize
        || values.len() != before.1 as usize
    {
        return Err("notification_registry_changed_kept");
    }
    Ok(Contents { values, children })
}
struct Local;
impl Registry for Local {
    fn read(&mut self, path: &str) -> Result<Option<Contents>, &'static str> {
        open(path)?.map(|key| snapshot(key.0)).transpose()
    }
    fn create_new(
        &mut self,
        path: &str,
        values: &BTreeMap<String, String>,
    ) -> Result<(), &'static str> {
        let mut raw = ptr::null_mut();
        let mut disposition = 0;
        let result = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(path).as_ptr(),
                0,
                ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_SET_VALUE | KEY_WOW64_64KEY,
                ptr::null(),
                &mut raw,
                &mut disposition,
            )
        };
        if result != ERROR_SUCCESS {
            return Err("notification_registry_create_unavailable");
        }
        let key = Key(raw);
        if disposition != REG_CREATED_NEW_KEY {
            return Err("notification_registry_create_race_kept");
        }
        for (name, value) in values {
            let data = wide(value);
            if data.len() > 32768 {
                return Err("notification_registry_value_refused");
            }
            if unsafe {
                RegSetValueExW(
                    key.0,
                    wide(name).as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr().cast(),
                    (data.len() * 2) as u32,
                )
            } != ERROR_SUCCESS
            {
                return Err("notification_registry_write_unavailable");
            }
        }
        Ok(())
    }
    fn remove_exact(&mut self, path: &str, expected: &Contents) -> Result<(), &'static str> {
        let key = open(path)?.ok_or("notification_registry_disappeared_kept")?;
        if snapshot(key.0)? != *expected {
            return Err("notification_registry_changed_kept");
        }
        if unsafe { RegDeleteKeyExW(HKEY_CURRENT_USER, wide(path).as_ptr(), KEY_WOW64_64KEY, 0) }
            != ERROR_SUCCESS
        {
            return Err("notification_registry_remove_unavailable");
        }
        Ok(())
    }
}
fn fingerprint(path: &Path) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= 256 * 1024 * 1024,
        "selected CLI exceeds its bound"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut count = 0;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += n;
        ensure!(count <= 256 * 1024 * 1024, "selected CLI changed");
        hash.update(&buffer[..n]);
    }
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len()
            && count as u64 == after.len()
            && before.modified()? == after.modified()?,
        "selected CLI changed"
    );
    Ok(hash.finalize().into())
}
struct RemovalGuard {
    _owner: Option<platform::FileLock>,
    _handoff: crate::desktop::store::HandoffLock,
    _request_id: String,
    _runtime_epoch: Option<String>,
}
impl RemovalGuard {
    fn verified(location: &Location, id: &str) -> Result<Self> {
        ensure!(
            zerus_desktop_migration::canonical_uuid(id),
            "a retained original revoke request UUID is required"
        );
        let status = crate::desktop::status(location)?;
        ensure!(
            status["configuration"] == location.configuration,
            "desktop configuration changed"
        );
        let online = status["running"]
            .as_bool()
            .context("desktop owner state is unavailable")?;
        if online {
            let epoch = status["epoch"]
                .as_str()
                .context("original runtime epoch is unavailable")?;
            let reply = crate::desktop::exchange(
                location,
                "request",
                Some(serde_json::json!({"op":"revoke_status","request_id":id})),
            )?
            .context("original service is unavailable; keep registration")?;
            ensure!(settled_revoke(&reply,id,&location.configuration,epoch),"original revoke is not settled; keep registration and query its original request UUID");
            // This is not a second revoke. Fence a new handoff while removing
            // the registration, and retain the exact original runtime epoch.
            let held = location.handoff_lock()?;
            let marker = crate::desktop::handoff::marker(location, &held)?;
            ensure!(
                marker.is_none_or(|v| v.phase == zerus_desktop_migration::Phase::Revoked),
                "a new handoff superseded revocation; keep registration"
            );
            let now = crate::desktop::status(location)?;
            ensure!(
                now["epoch"].as_str() == Some(epoch)
                    && now["configuration"] == location.configuration
                    && now["running"] == true,
                "original desktop owner changed; keep registration"
            );
            // The published component is not yet attached to the service's
            // Windows drain. A generic settled receipt cannot stand in for
            // that original Runtime/COM/delegate join. Root integration must
            // supply its actual matching witness before this path is enabled.
            bail!("active Windows notification removal awaits integrated Runtime drain; registration kept");
        }
        // No missing endpoint is treated as a stopped owner. The actual OS lock
        // must be acquired; existing state cannot be replaced with a baseline.
        location.prepare()?;
        let owner = location.lock()?;
        let held = location.handoff_lock()?;
        for name in [
            "state.sqlite3",
            "state.sqlite3-wal",
            "state.sqlite3-shm",
            "state.sqlite3-journal",
            zerus_desktop_migration::HANDOFF_FILE,
        ] {
            match std::fs::symlink_metadata(location.root.join(name)) {
                Err(error) if error.kind()==std::io::ErrorKind::NotFound=>(),
                _=>bail!("retained desktop state requires its original settled service receipt; registration kept"),
            }
        }
        // Verified absent-service/no-state is not a live Runtime drain receipt.
        Ok(Self {
            _owner: Some(owner),
            _handoff: held,
            _request_id: id.into(),
            _runtime_epoch: None,
        })
    }
}
pub(super) fn command(
    config: &Config,
    operation: &str,
    request_id: Option<&str>,
) -> Result<serde_json::Value> {
    let location = Location::new(config)?;
    let service = registration::windows_plan(config, &location)?;
    let sid = host::token::user_sid()?;
    ensure!(
        service["user_sid"].as_str() == Some(&sid),
        "registration user changed"
    );
    let args = service["argv"]
        .as_array()
        .context("desktop registration arguments unavailable")?;
    ensure!(args.len() == 7, "desktop registration arguments changed");
    let cli = args[0].as_str().context("selected CLI unavailable")?;
    let mut command = format!("\"{cli}\"");
    for arg in [
        super::super::ACTIVATION_ENTRY,
        "--config-dir",
        args[4].as_str().context("configuration unavailable")?,
        "--state-dir",
        args[6].as_str().context("state unavailable")?,
    ] {
        command.push(' ');
        command.push_str(&host::spawn::quote_argument(arg));
    }
    let plan =
        Plan::new(&location.configuration, &sid, &command).map_err(|v| anyhow::anyhow!(v))?;
    let _lock = Lock::acquire(&plan.sid)?;
    let mut registry = Local;
    plan.inspect(&mut registry)
        .map_err(|v| anyhow::anyhow!(v))?;
    if operation == "install" {
        // The service installer stages the stable CLI first. Comparing held bounded
        // files ensures a stale selected CLI cannot receive an unknown cold flag.
        let cli = platform::canonical(Path::new(cli))
            .context("install the maintained desktop registration before Toast registration")?;
        ensure!(fingerprint(&cli)?==fingerprint(&std::env::current_exe()?)?,"selected CLI differs; install the maintained desktop registration before Toast registration");
    }
    let _removal = if operation == "remove" {
        Some(RemovalGuard::verified(
            &location,
            request_id.context("remove requires the retained original revoke request UUID")?,
        )?)
    } else {
        ensure!(request_id.is_none(), "unexpected revoke request UUID");
        None
    };
    let reply = plan.apply(&mut registry, operation).map_err(|failure| {
        anyhow::anyhow!(
            "{}{}",
            failure.reason,
            if failure.uncertain {
                "; outcome unknown, inspect status before retrying"
            } else {
                "; registration kept"
            }
        )
    })?;
    Ok(
        serde_json::json!({"schema":1,"abi":MARKER,"operation":operation,"configuration":plan.configuration,"registered":reply.registered,"changed":reply.changed,"started":false,"stopped":false}),
    )
}
