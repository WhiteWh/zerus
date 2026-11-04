//! A startup refusal gets a visible native dialog without constructing the
//! client source, Bootstrap, notifier, or any background writer.
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

/// One small private record, replaced atomically. No child text, environment,
/// request/configuration identity or user content enters this file or stderr.
pub(crate) fn record(step: &'static str, code: &'static str) {
    eprintln!(
        "zerus-client: desktop startup refused; step={step} code={code}; writers remain stopped"
    );
    let absolute = |key| {
        std::env::var_os(key)
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base =
        absolute("XDG_STATE_HOME").or_else(|| absolute("HOME").map(|p| p.join(".local/state")));
    if let Some(base) = base {
        let _ = write_record(&base.join("hgs/client-startup-refusal.json"), step, code);
    }
}

fn write_record(path: &std::path::Path, step: &str, code: &str) -> std::io::Result<()> {
    use std::io::Write;
    let recorded_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(std::io::Error::other)?
        .as_millis();
    let body = serde_json::to_vec(&serde_json::json!({
        "version":1,"step":step,"code":code,"recorded_at_unix_ms":recorded_at_unix_ms
    }))?;
    if body.len() > 512 {
        return Err(std::io::Error::other("startup diagnostic exceeds limit"));
    }
    let folder = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing diagnostic directory"))?;
    std::fs::create_dir_all(folder)?;
    let temporary = folder.join(format!(".startup-refusal-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn show(context: tauri::Context<tauri::Wry>) {
    let title = crate::native_text::render(&zerus_client_core::text!("Zerus could not start"));
    let message = crate::native_text::render(&zerus_client_core::text!(
        "Zerus could not confirm desktop ownership. Background work has not started. Close this message, then start Zerus again through its normal launcher. If the problem continues, check the desktop service."
    ));
    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let quit = app.handle().clone();
            app.dialog()
                .message(message)
                .title(title)
                .kind(MessageDialogKind::Error)
                .show(move |_| quit.exit(0));
            Ok(())
        })
        .run(context);
    if let Err(error) = result {
        eprintln!("zerus-client: could not show startup error: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_record_is_bounded_private_and_replaces_a_symlink_without_following_it() {
        let root = std::env::temp_dir().join(format!("startup-record-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("refusal.json");
        let outside = root.join("unrelated");
        std::fs::write(&outside, "kept").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        write_record(
            &path,
            "native_takeover.frontend_request",
            "frontend_process_image_unavailable",
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "kept");
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() <= 512);
        let mut record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let timestamp = record
            .as_object_mut()
            .unwrap()
            .remove("recorded_at_unix_ms")
            .unwrap()
            .as_u64()
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        assert!(timestamp > 0 && u128::from(timestamp) <= now);
        assert_eq!(
            record,
            serde_json::json!({
                "version":1,"step":"native_takeover.frontend_request","code":"frontend_process_image_unavailable"
            })
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(write_record(&path, &"x".repeat(513), "child_failed").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }
}
