//! Read one original send outcome. Names are original receipt data, never a
//! mutable route to a live session or a reason to initialize a provider.
use super::*;
use std::io::Read;

pub(super) fn dispatch(args: &[String]) -> Result<i32> {
    let [operation, name, format] = args else {
        return Err("usage: hgs delivery receipt <original-session-name> --json".into());
    };
    if operation != "receipt" || format != "--json" {
        return Err("usage: hgs delivery receipt <original-session-name> --json".into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| "Invalid delivery receipt query")?;
    if bytes.len() > 8192 {
        return Err("Delivery receipt query exceeds 8 KiB".into());
    }
    let query: receipt_index::DeliveryQuery =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid delivery receipt query")?;
    query.validate(name)?;
    let config =
        crate::config::Config::load().map_err(|_| "Delivery receipt computer is unavailable")?;
    let machine = session_action::status_machine(&config)
        .map_err(|_| "Delivery receipt computer identity is unavailable")?;
    if machine != query.machine() {
        return Err("Delivery receipt computer changed".into());
    }
    println!("{}", receipt_index::delivery_receipt(&root(), name, &query));
    Ok(0)
}

pub(super) fn bind(receipt: &mut Value, record: &Value, target: &Value) {
    if let Some(binding) = receipt_index::delivery_binding(record, target) {
        receipt["delivery_binding"] = binding;
    }
}
