//! One bounded, serial read-only CLI relay. Every page still uses ordinary
//! authenticated local IPC and the actor's original one-use cursor checks.
use super::{exchange, protocol, Location};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use zerus_client_core::desktop::service::RELAY_ABI;

pub(super) fn run(location: &Location) -> Result<()> {
    relay(
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &location.configuration,
        |request| {
            exchange(location, "request", Some(request))?.context("desktop service is not running")
        },
    )
}

fn relay(
    input: &mut impl BufRead,
    output: &mut impl Write,
    configuration: &str,
    mut read: impl FnMut(Value) -> Result<Value>,
) -> Result<()> {
    output.write_all(&protocol::encode(
        &json!({"abi":RELAY_ABI,"configuration":configuration}),
    )?)?;
    output.flush()?;
    while let Some(bytes) = protocol::line(input, protocol::CONTROL_BYTES)? {
        let request: Value =
            serde_json::from_slice(&bytes).context("invalid catalog relay request")?;
        // No generic or authenticated frontend mutation is reachable through
        // this command, including when an untrusted caller controls stdin.
        ensure!(
            matches!(
                request["op"].as_str(),
                Some("catalog_page" | "catalog_changes" | "catalog_close")
            ),
            "catalog relay accepts only catalog reads"
        );
        let id = request["request_id"]
            .as_str()
            .context("missing catalog request identity")?
            .to_owned();
        ensure!(
            uuid::Uuid::parse_str(&id).is_ok_and(|id_value| id_value.to_string() == id),
            "invalid catalog request identity"
        );
        let response = read(request)?;
        ensure!(
            response["request_id"] == id,
            "catalog response identity changed"
        );
        output.write_all(&protocol::encode(
            &json!({"abi":RELAY_ABI,"request_id":id,"value":response}),
        )?)?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn serial_pages_preserve_identity_and_stop_on_unknown_outcome() {
        let id = uuid::Uuid::new_v4().to_string();
        let line = format!(
            "{}\n",
            json!({"op":"catalog_page","request_id":id,"computer":"","cursor":null})
        );
        let input = line.repeat(3);
        let mut calls = 0;
        let mut output = Vec::new();
        assert!(relay(
            &mut input.as_bytes(),
            &mut output,
            "configuration",
            |request| {
                calls += 1;
                if calls == 2 {
                    anyhow::bail!("unknown exchange");
                }
                Ok(json!({"request_id":request["request_id"],"accepted":true,"items":[]}))
            }
        )
        .is_err());
        assert_eq!(calls, 2, "unknown outcomes never resend or continue");
        let lines: Vec<Value> = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["configuration"], "configuration");
        assert_eq!(lines[1]["request_id"], id);
        assert_eq!(lines[1]["value"]["items"], json!([]));
    }
    #[test]
    fn closed_read_lane_refuses_mutations_truncation_and_oversized_input_or_output() {
        let id = uuid::Uuid::new_v4().to_string();
        for op in [
            "automatic_approvals",
            "maintenance_configure",
            "revoke",
            "read_intent",
            "frontend_request",
            "detail_follow",
        ] {
            let input = format!("{}\n", json!({"op":op,"request_id":id}));
            assert!(
                relay(&mut input.as_bytes(), &mut Vec::new(), "", |_| panic!(
                    "mutation reached IPC"
                ))
                .is_err()
            );
        }
        for input in ["x".repeat(protocol::CONTROL_BYTES + 1), "{}".into()] {
            assert!(
                relay(&mut input.as_bytes(), &mut Vec::new(), "", |_| panic!(
                    "invalid input reached IPC"
                ))
                .is_err()
            );
        }
        let input = format!("{}\n", json!({"op":"catalog_changes","request_id":id}));
        assert!(relay(&mut input.as_bytes(), &mut Vec::new(), "", |_| Ok(
            json!({"request_id":id,"body":"x".repeat(protocol::FRAME_BYTES)})
        ))
        .is_err());
        assert!(relay(&mut input.as_bytes(), &mut Vec::new(), "", |_| Ok(
            json!({"request_id":uuid::Uuid::new_v4().to_string()})
        ))
        .is_err());
    }
}
