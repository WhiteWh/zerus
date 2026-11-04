//! One read-only canonical instance challenge, without service activation.
use super::store::Location;
use serde_json::{json, Value};
use zerus_client_core::desktop;
#[cfg(not(unix))]
use zerus_client_core::desktop::Observation;

#[cfg(unix)]
pub(super) fn instance_path() -> std::path::PathBuf {
    desktop::temporary_dir_from(std::env::var_os("TMPDIR")).join("hgs-tray")
}

pub fn status(location: &Location) -> Value {
    #[cfg(unix)]
    let observation = {
        let path = instance_path();
        desktop::probe_at(
            &path,
            &uuid::Uuid::new_v4().to_string(),
            &location.configuration,
        )
    };
    // Windows canonical instance ownership/locking needs separate native proof.
    #[cfg(not(unix))]
    let observation = Observation::Unknown;
    json!({"abi":desktop::ABI,"configuration":location.configuration,
        "observation":observation,"supported":cfg!(unix),"authority":false,
        "writers_enabled":false,"takeover":false})
}
