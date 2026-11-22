//! Optional paged-archive previews share the existing computer packet budget.
//! Current rows and other metadata are never candidates for removal.
use super::{alerts, encoded, Failure, Result, Value};

const PLAIN_BYTES: usize = 1024 * 1024;
const PREVIEW_ROWS: usize = 50;

fn preview(row: &Value) -> Option<(Vec<usize>, u64)> {
    let snapshot = row.get("snapshot")?.as_object()?;
    let metadata = snapshot.get("mobile_archive")?.as_object()?;
    if metadata.len() != 5
        || metadata.get("schema")?.as_u64()? != 1
        || !metadata.get("available")?.as_bool()?
    {
        return None;
    }
    let loaded = metadata.get("loaded")?.as_u64()?;
    let total = metadata.get("total")?.as_u64()?;
    if loaded > PREVIEW_ROWS as u64
        || loaded == 0
        || total < loaded
        || metadata.get("partial")?.as_bool()? != (total > loaded)
    {
        return None;
    }
    let mut indices = Vec::new();
    let mut identities = std::collections::HashSet::new();
    for (index, session) in snapshot.get("sessions")?.as_array()?.iter().enumerate() {
        if !alerts::archived(session) {
            continue;
        }
        let archive = session.get("archive_id")?.as_str()?;
        let name = session.get("name")?.as_str()?;
        if indices.len() == PREVIEW_ROWS
            || name.is_empty()
            || !crate::pyvalue::canonical_uuid(archive)
            || !identities.insert(archive)
        {
            return None;
        }
        indices.push(index);
    }
    (indices.len() as u64 == loaded).then_some((indices, total))
}

struct Selection {
    // Only the at-most-50 optional rows move aside. Current rows are not cloned.
    entries: Vec<(usize, Option<Value>)>,
    kept: usize,
}
impl Selection {
    fn new(row: &mut Value, indices: Vec<usize>) -> Self {
        let sessions = row["snapshot"]["sessions"].as_array_mut().unwrap();
        let original = std::mem::take(sessions);
        let mut indices = indices.into_iter().peekable();
        let mut entries = Vec::new();
        for (index, session) in original.into_iter().enumerate() {
            if indices.peek() == Some(&index) {
                indices.next();
                entries.push((index, Some(session)));
            } else {
                sessions.push(session);
            }
        }
        Self { entries, kept: 0 }
    }

    fn select(&mut self, row: &mut Value, count: usize, total: u64) {
        let sessions = row["snapshot"]["sessions"].as_array_mut().unwrap();
        // Prefix positions remain the original positions even with current
        // rows interleaved. Remove in reverse; insert forwards at those pins.
        for (index, value) in self.entries[..self.kept].iter_mut().rev() {
            *value = Some(sessions.remove(*index));
        }
        for (index, value) in &mut self.entries[..count] {
            sessions.insert(*index, value.take().unwrap());
        }
        self.kept = count;
        row["snapshot"]["mobile_archive"]["loaded"] = serde_json::json!(count);
        row["snapshot"]["mobile_archive"]["partial"] = serde_json::json!(total > count as u64);
    }
}

pub(super) fn fit(row: &mut Value, original: Vec<u8>) -> Result<Vec<u8>> {
    if original.len() <= PLAIN_BYTES {
        return Ok(original);
    }
    let (indices, total) = preview(row).ok_or(Failure::Retry)?;
    drop(original);
    let mut selection = Selection::new(row, indices);
    selection.select(row, 0, total);
    let mut best = encoded(row)?;
    if best.len() > PLAIN_BYTES {
        return Err(Failure::Retry);
    }
    let mut low = 0;
    // The complete original prefix already exceeded the bound. At most six
    // trial encodes find the largest fitting prefix; the cap never changes.
    let mut high = selection.entries.len();
    while low + 1 < high {
        let count = low + (high - low) / 2;
        selection.select(row, count, total);
        let candidate = encoded(row)?;
        if candidate.len() <= PLAIN_BYTES {
            low = count;
            best = candidate;
        } else {
            high = count;
        }
    }
    selection.select(row, low, total);
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn packet(room: usize) -> Value {
        let mut value = json!({"id":"fixture-computer","snapshot":{
            "sessions":[{"name":"fixture/main","state":"running","reply_id":"original-reply",
                "activity":"working","phase":"working","mobile_unread_reply":false}],
            "mobile_archive":{"schema":1,"available":true,"loaded":0,"total":3,"partial":true}},
            "padding":""});
        let empty = encoded(&value).unwrap().len();
        value["padding"] = json!("x".repeat(PLAIN_BYTES - empty - room));
        value
    }

    fn archived(index: usize) -> Value {
        json!({"name":"fixture/main","state":"archived",
            "archive_id":format!("00000000-0000-4000-8000-{index:012}"),
            "last_message":"é\n\"🦀".repeat(130)})
    }

    fn append_previews(value: &mut Value, count: usize) {
        value["snapshot"]["sessions"]
            .as_array_mut()
            .unwrap()
            .extend((1..=count).map(archived));
        value["snapshot"]["mobile_archive"]["loaded"] = json!(count);
        value["snapshot"]["mobile_archive"]["partial"] = json!(count < 3);
    }

    #[test]
    fn fitting_packet_is_byte_and_value_unchanged() {
        let mut row = packet(10000);
        append_previews(&mut row, 3);
        let before = row.clone();
        let plain = encoded(&row).unwrap();
        assert!(plain.len() < PLAIN_BYTES);
        assert_eq!(fit(&mut row, plain.clone()).unwrap(), plain);
        assert_eq!(row, before);
    }

    #[test]
    fn no_optional_preview_can_fit_and_archive_stays_available_with_exact_total() {
        let mut row = packet(10);
        let current = row["snapshot"]["sessions"][0].clone();
        append_previews(&mut row, 3);
        let original = encoded(&row).unwrap();
        assert!(original.len() > PLAIN_BYTES);
        let plain = fit(&mut row, original).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&plain).unwrap(), row);
        assert!(plain.len() <= PLAIN_BYTES);
        assert_eq!(row["snapshot"]["sessions"], json!([current]));
        assert_eq!(
            row["snapshot"]["mobile_archive"],
            json!({"schema":1,"available":true,"loaded":0,"total":3,"partial":true})
        );
    }

    #[test]
    fn interleaved_current_rows_survive_and_utf8_encoded_budget_keeps_the_largest_prefix() {
        let mut row = packet(3000);
        append_previews(&mut row, 3);
        let second = json!({"name":"fixture/second","state":"paused","activity":"unknown",
            "reply_id":"different-reply","allowed_actions":["inspect"]});
        row["snapshot"]["sessions"]
            .as_array_mut()
            .unwrap()
            .insert(2, second.clone());
        let before = row.clone();
        let original = encoded(&row).unwrap();
        assert!(original.len() > PLAIN_BYTES);
        let plain = fit(&mut row, original).unwrap();
        assert!(plain.len() <= PLAIN_BYTES);
        assert_eq!(serde_json::from_slice::<Value>(&plain).unwrap(), row);
        let rows = row["snapshot"]["sessions"].as_array().unwrap();
        assert_eq!(rows[0], before["snapshot"]["sessions"][0]);
        assert_eq!(
            rows.iter()
                .filter(|v| !alerts::archived(v))
                .cloned()
                .collect::<Vec<_>>(),
            vec![before["snapshot"]["sessions"][0].clone(), second]
        );
        let kept = row["snapshot"]["mobile_archive"]["loaded"]
            .as_u64()
            .unwrap() as usize;
        assert!(kept > 0 && kept < 3);
        assert_eq!(
            rows.iter()
                .filter(|v| alerts::archived(v))
                .cloned()
                .collect::<Vec<_>>(),
            (1..=kept).map(archived).collect::<Vec<_>>()
        );
        let mut next = row.clone();
        next["snapshot"]["sessions"]
            .as_array_mut()
            .unwrap()
            .push(archived(kept + 1));
        next["snapshot"]["mobile_archive"]["loaded"] = json!(kept + 1);
        next["snapshot"]["mobile_archive"]["partial"] = json!(kept + 1 < 3);
        assert!(encoded(&next).unwrap().len() > PLAIN_BYTES);
    }

    #[test]
    fn uncertain_preview_metadata_or_identity_never_admits_truncation() {
        let mut original = packet(3000);
        append_previews(&mut original, 3);
        assert!(encoded(&original).unwrap().len() > PLAIN_BYTES);
        let mut invalid = Vec::new();
        for (field, value) in [
            ("schema", json!(2)),
            ("available", json!(false)),
            ("loaded", json!(2)),
            ("total", json!(2)),
            ("partial", json!(true)),
            ("loaded", json!(true)),
        ] {
            let mut row = original.clone();
            row["snapshot"]["mobile_archive"][field] = value;
            invalid.push(row);
        }
        let mut extra = original.clone();
        extra["snapshot"]["mobile_archive"]["future_digest"] = json!("unknown");
        invalid.push(extra);
        let mut duplicate = original.clone();
        duplicate["snapshot"]["sessions"][2]["archive_id"] =
            duplicate["snapshot"]["sessions"][1]["archive_id"].clone();
        invalid.push(duplicate);
        let mut missing = original.clone();
        missing["snapshot"]["sessions"][1]
            .as_object_mut()
            .unwrap()
            .remove("archive_id");
        invalid.push(missing);
        let mut excessive = original;
        excessive["snapshot"]["sessions"]
            .as_array_mut()
            .unwrap()
            .extend((4..=51).map(archived));
        excessive["snapshot"]["mobile_archive"]["loaded"] = json!(51);
        excessive["snapshot"]["mobile_archive"]["total"] = json!(51);
        invalid.push(excessive);
        for mut row in invalid {
            let bytes = encoded(&row).unwrap();
            assert!(matches!(fit(&mut row, bytes), Err(Failure::Retry)));
        }
    }
}
