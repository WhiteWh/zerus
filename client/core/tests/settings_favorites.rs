//! Favorites use the native local preference writer, independently of names.
use serde_json::json;
use std::{fs, time::Duration};
use zerus_client_core::settings::Settings;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn favorites_survive_native_settings_restart_and_removal() {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("zerus-favorites-{}", std::process::id())));
    fs::create_dir(&scratch.0).unwrap();
    let path = scratch.0.join("settings.json");
    let identity = "11111111-1111-4111-8111-111111111111\n22222222-2222-4222-8222-222222222222";
    let value = json!([identity]).to_string();
    let store = Settings::open(path.clone());
    store.set("sessions/favorites", json!(value)).unwrap();
    assert!(store.flush_within(Duration::from_secs(5)));
    drop(store);
    let restarted = Settings::open(path.clone());
    assert_eq!(restarted.get("sessions/favorites"), json!(value));
    restarted.set("sessions/favorites", json!("[]")).unwrap();
    assert!(restarted.flush_within(Duration::from_secs(5)));
    drop(restarted);
    assert_eq!(
        Settings::open(path.clone()).get("sessions/favorites"),
        json!("[]")
    );
    assert!(fs::read_to_string(path)
        .unwrap()
        .contains("sessions/favorites"));
}
