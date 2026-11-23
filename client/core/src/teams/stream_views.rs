//! Task #16 of team c536f0e5 (P2.5): the `team.views` rows of one computer's
//! watch stream. Each row is exactly the reply of one Teams read (`list`,
//! `waiting`, `status:<id>`, `inbox:<id>`, `feed:<id>`), so the
//! [`crate::TeamsModel`] applies it as it applies that read's reply
//! ([`crate::TeamsModel::views`]) and runs no `hgs team` for it.
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use zerus_watch_protocol::{Delta, Snapshot};

/// Rows as they arrive: a complete snapshot (every row) or a delta's upserts.
pub type ViewRows = Arc<Vec<(String, Value)>>;

/// The team views of one stream.
#[derive(Debug, Default)]
pub struct TeamViewRows {
    /// Whether a complete snapshot arrived and nothing was lost since.
    complete: bool,
    staged: Option<BTreeMap<String, Value>>,
}

impl TeamViewRows {
    /// A snapshot page; every row once its last page arrived.
    pub fn snapshot(&mut self, snapshot: &Snapshot) -> Option<ViewRows> {
        if snapshot.page == 0 {
            self.staged = Some(BTreeMap::new());
        }
        // A page without its first is a broken snapshot: wait for the next.
        let staged = self.staged.as_mut()?;
        for item in snapshot.items.iter().flatten() {
            if let Some(key) = item["key"].as_str() {
                staged.insert(key.to_owned(), item["row"].clone());
            }
        }
        if !snapshot.last {
            return None;
        }
        self.complete = true;
        Some(Arc::new(self.staged.take()?.into_iter().collect()))
    }

    /// A delta's changed rows, when a snapshot came before it.
    pub fn delta(&mut self, delta: &Delta) -> Option<ViewRows> {
        if !self.complete || delta.upsert.is_empty() {
            return None;
        }
        Some(Arc::new(delta.upsert.iter().map(|item| (item.key.clone(), item.row.clone())).collect()))
    }

    /// A gap, a failure or a new stream: true when the views were followed
    /// until now (the model reads them with their commands again).
    pub fn reset(&mut self) -> bool {
        self.staged = None;
        std::mem::take(&mut self.complete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zerus_watch_protocol::{topic, CatalogItem};

    fn page(items: Vec<Value>, page: u32, last: bool) -> Snapshot {
        Snapshot { topic: topic::TEAM_VIEWS.into(), key: None, items: Some(items), value: None, page, last, cursor: 1 }
    }

    #[test]
    fn a_snapshot_gives_every_row_and_a_delta_its_upserts() {
        let mut rows = TeamViewRows::default();
        let delta = Delta { topic: topic::TEAM_VIEWS.into(), upsert: vec![CatalogItem { key: "list".into(), row: json!({"items":[]}) }], remove: vec![], cursor: 2 };
        assert!(rows.delta(&delta).is_none(), "nothing before a snapshot");
        assert!(rows.snapshot(&page(vec![json!({"key":"list","row":{"items":[1]}})], 0, false)).is_none());
        let all = rows.snapshot(&page(vec![json!({"key":"waiting","row":{"items":[]}})], 1, true)).unwrap();
        assert_eq!(all.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>(), ["list", "waiting"]);
        let changed = rows.delta(&delta).unwrap();
        assert_eq!(*changed, vec![("list".to_owned(), json!({"items":[]}))]);
        assert!(rows.reset(), "they were followed");
        assert!(!rows.reset());
        assert!(rows.delta(&delta).is_none(), "lost until the next snapshot");
        assert!(rows.snapshot(&page(Vec::new(), 1, true)).is_none(), "a page without its first");
    }
}
