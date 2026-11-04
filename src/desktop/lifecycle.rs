//! Installer and UI readiness for the existing desktop-v1 endpoint.
//! IPC readiness is independent of background writer ownership. Starting the
//! passive kernel must never advertise workers which have not been started.
use serde_json::{json, Value};

pub(super) const ABI: &str = "desktop-lifecycle-v1";

pub(super) fn unavailable(configuration: &str) -> Value {
    // A missing endpoint is not proof that every old process has exited.
    json!({"abi":ABI,"phase":"unavailable","ipc_ready":false,
        "pid":null,"configuration":configuration,"epoch":null,"generation":null})
}

pub(super) fn status(configuration: &str, epoch: &str, generation: u64) -> Value {
    json!({
        "abi": ABI,
        "phase": "serving",
        "ipc_ready": true,
        "pid": std::process::id(),
        "configuration": configuration,
        "epoch": epoch,
        "generation": generation,
        "background": {
            "fleet": {"ready": false, "code": "not_started"},
            "presence": {"ready": false, "code": "not_started"},
            "reads": {"ready": false, "code": "not_started"},
            "attention": {"ready": false, "code": "not_started"},
            "approvals": {"ready": false, "code": "not_started"}
        }
    })
}

/// Probe the same selected binary/configuration, even for a nondefault install.
/// The installer supplies this environment to a local child; it is not a shell
/// command, a captured ambient environment, or a new service registration.
pub(super) fn probe(binary: &str, config: &str, state: &str, configuration: &str) -> Value {
    json!({
        "abi": ABI,
        "argv": [binary, "desktop", "status", "--json"],
        "environment": {"HGS_CONFIG_DIR": config, "HGS_STATE_DIR": state},
        "expected_configuration": configuration,
        "configuration_field": "lifecycle.configuration",
        "ready_field": "lifecycle.ipc_ready",
        "epoch_field": "lifecycle.epoch",
        "generation_field": "lifecycle.generation"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_readiness_does_not_claim_background_ownership() {
        let value = status("configuration", "epoch-one", 7);
        assert_eq!(value["ipc_ready"], true);
        assert_eq!(value["pid"], std::process::id());
        assert_eq!(value["configuration"], "configuration");
        assert_eq!(value["epoch"], "epoch-one");
        assert_eq!(value["generation"], 7);
        for component in ["fleet", "presence", "reads", "attention", "approvals"] {
            assert_eq!(value["background"][component]["ready"], false);
            assert_eq!(value["background"][component]["code"], "not_started");
        }
    }

    #[test]
    fn missing_endpoint_is_not_a_process_exit_receipt() {
        let value = unavailable("configuration");
        assert_eq!(value["ipc_ready"], false);
        assert_eq!(value["phase"], "unavailable");
        assert!(value["pid"].is_null());
        assert!(value["epoch"].is_null());
        assert!(value["generation"].is_null());
    }

    #[test]
    fn nondefault_probe_preserves_argv_paths_without_shell_expansion() {
        let value = probe(
            "/opt/example/hgs cli",
            "/private/config $literal",
            "/private/state",
            "expected",
        );
        assert_eq!(
            value["argv"],
            json!(["/opt/example/hgs cli", "desktop", "status", "--json"])
        );
        assert_eq!(
            value["environment"]["HGS_CONFIG_DIR"],
            "/private/config $literal"
        );
        assert_eq!(value["environment"]["HGS_STATE_DIR"], "/private/state");
        assert_eq!(value["environment"].as_object().unwrap().len(), 2);
        assert_eq!(value["expected_configuration"], "expected");
    }
}
