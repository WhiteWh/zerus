//! Encoding explicitly selected, validated row data. These values are output,
//! never the original raw spans or G3 source evidence.
use super::*;
use serde_json::{json, Map, Value};
#[cfg(test)]
mod tests;

pub(super) fn encode(
    saved: &OwnedSaved,
    expected: Owner,
    rows: &data::Rows,
) -> Result<Vec<u8>, Refused> {
    if saved.has_unknown || rows.has_unknown() {
        return Err(Refused::UnknownFields);
    }
    let mut tokens = Map::new();
    let mut uncertain = Vec::new();
    for row in rows.claims() {
        let value =
            serde_json::from_slice::<Value>(&row.encode()?).map_err(|_| Refused::Malformed)?;
        match row.placement() {
            data::Placement::Tokens(id) => {
                tokens.insert(id.to_string(), value);
            }
            data::Placement::Uncertain => uncertain.push(value),
        }
    }
    let ns = saved.namespace();
    let mut value = json!({"version":2,"bus_id":ns.map_or("",|n|n.bus_id.as_str()),
        "owner":ns.map_or("",|n|n.owner.as_str()),"tokens":tokens,"uncertain":uncertain,"legacy_uncertain":[]});
    if expected == Owner::SoftwareUpdate {
        let invalidations = rows
            .invalidations()
            .iter()
            .map(|row| {
                serde_json::from_slice::<Value>(&row.encode()?).map_err(|_| Refused::Malformed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        value["invalidations"] = Value::Array(invalidations);
    }
    let mut out = Capped(Vec::new());
    serde_json::to_writer(&mut out, &value).map_err(|_| Refused::TooLarge)?;
    Ok(out.0)
}
