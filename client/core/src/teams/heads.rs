//! The `team` topic of a computer's watch stream (task 8): every team's head
//! as keyed rows (`team:<id>` with its `revision`). [`TeamHeads`] keeps the
//! rows a stream sent and gives the revisions by team once they are
//! complete; the Teams poller then reads a team again only when its revision
//! moved ([`crate::TeamsModel::heads`]). Until a snapshot is complete, after
//! a `gap` or a failure, and on a stream without the topic, there are no
//! heads and the poller polls as before.
//!
//! Task #6 of team c536f0e5 (P1.4): a head also keeps a digest of its row
//! without `revision` and `updated`, so the poller tells a change the list
//! shows (a name, a status, a mode) from a revision that moved alone.
use std::collections::hash_map::DefaultHasher;
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde_json::Value;
use zerus_watch_protocol::{Delta, Snapshot};

/// One team's head: its revision and the digest of the rest of its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub revision: i64,
    /// The row without `revision` and `updated`.
    pub digest: u64,
}

impl Head {
    /// The head of a `team:<id>` row; None without a revision.
    pub fn of(row: &Value) -> Option<Self> {
        let revision = row["revision"].as_i64()?;
        let mut rest = row.clone();
        if let Some(fields) = rest.as_object_mut() {
            fields.remove("revision");
            fields.remove("updated");
        }
        let mut hasher = DefaultHasher::new();
        rest.to_string().hash(&mut hasher);
        Some(Self {
            revision,
            digest: hasher.finish(),
        })
    }
}

/// A team's head by its id.
pub type Heads = BTreeMap<String, Head>;

/// Whether two sets of heads differ in more than revisions (and `updated`):
/// a team came or left, or a row's other fields changed.
pub fn changed_beyond_revisions(old: &Heads, new: &Heads) -> bool {
    old.len() != new.len()
        || old
            .iter()
            .zip(new)
            .any(|((old_id, old), (new_id, new))| old_id != new_id || old.digest != new.digest)
}

/// The team rows of one stream.
#[derive(Debug, Default)]
pub struct TeamHeads {
    rows: Option<BTreeMap<String, Value>>,
    staged: Option<BTreeMap<String, Value>>,
}

impl TeamHeads {
    /// A snapshot page; the heads once its last page arrived.
    pub fn snapshot(&mut self, snapshot: &Snapshot) -> Option<Arc<Heads>> {
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
        self.rows = self.staged.take();
        self.heads()
    }

    /// A delta; the heads when a snapshot came before it.
    pub fn delta(&mut self, delta: &Delta) -> Option<Arc<Heads>> {
        let rows = self.rows.as_mut()?;
        for removal in &delta.remove {
            rows.remove(&removal.key);
        }
        for item in &delta.upsert {
            rows.insert(item.key.clone(), item.row.clone());
        }
        self.heads()
    }

    /// A gap, a failure or a new stream: the rows are unknown until the next
    /// complete snapshot.
    pub fn reset(&mut self) {
        self.rows = None;
        self.staged = None;
    }

    /// The heads by team id; None while the rows are unknown.
    pub fn heads(&self) -> Option<Arc<Heads>> {
        let rows = self.rows.as_ref()?;
        Some(Arc::new(
            rows.iter()
                .filter_map(|(key, row)| {
                    Some((key.strip_prefix("team:")?.to_owned(), Head::of(row)?))
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zerus_watch_protocol::{CatalogItem, Removal};

    fn page(page: u32, last: bool, items: Vec<Value>) -> Snapshot {
        Snapshot {
            topic: "team".into(),
            key: None,
            items: Some(items),
            value: None,
            page,
            last,
            cursor: 1,
        }
    }

    fn row(id: &str, revision: i64) -> Value {
        json!({"key": format!("team:{id}"), "row": {"id": id, "revision": revision, "status": "running"}})
    }

    fn revisions(heads: &Heads) -> BTreeMap<String, i64> {
        heads
            .iter()
            .map(|(id, head)| (id.clone(), head.revision))
            .collect()
    }

    #[test]
    fn a_paged_snapshot_gives_heads_once_complete_then_deltas_change_them() {
        let mut heads = TeamHeads::default();
        assert_eq!(heads.heads(), None);
        assert_eq!(
            heads.snapshot(&page(0, false, vec![row("a", 3)])),
            None,
            "not complete yet"
        );
        let first = heads
            .snapshot(&page(
                1,
                true,
                vec![
                    row("b", 1),
                    json!({"key":"teams","row":{"count":2,"truncated":false}}),
                ],
            ))
            .unwrap();
        assert_eq!(
            revisions(&first),
            BTreeMap::from([("a".into(), 3), ("b".into(), 1)])
        );
        let delta = Delta {
            topic: "team".into(),
            upsert: vec![CatalogItem {
                key: "team:a".into(),
                row: json!({"id":"a","revision":4}),
            }],
            remove: vec![Removal {
                key: "team:b".into(),
                reason: None,
            }],
            cursor: 2,
        };
        assert_eq!(
            revisions(&heads.delta(&delta).unwrap()),
            BTreeMap::from([("a".into(), 4)])
        );
    }

    #[test]
    fn a_gap_or_a_broken_snapshot_leaves_no_heads_until_the_next_snapshot() {
        let mut heads = TeamHeads::default();
        heads.snapshot(&page(0, true, vec![row("a", 1)]));
        heads.reset();
        assert_eq!(heads.heads(), None);
        let delta = Delta {
            topic: "team".into(),
            upsert: vec![],
            remove: vec![],
            cursor: 3,
        };
        assert_eq!(heads.delta(&delta), None, "a delta needs a snapshot first");
        assert_eq!(
            heads.snapshot(&page(1, true, vec![row("a", 2)])),
            None,
            "a page without its first"
        );
        assert_eq!(
            revisions(&heads.snapshot(&page(0, true, vec![row("a", 2)])).unwrap()),
            BTreeMap::from([("a".into(), 2)])
        );
    }

    /// Task #6 of team c536f0e5 (P1.4): the digest leaves out `revision` and
    /// `updated` only.
    #[test]
    fn a_heads_digest_ignores_its_revision_and_updated_only() {
        let head = |row: Value| Head::of(&row).unwrap();
        let a = head(json!({"id":"a","revision":1,"updated":10.0,"status":"running"}));
        let b = head(json!({"id":"a","revision":2,"updated":13.5,"status":"running"}));
        let c = head(json!({"id":"a","revision":3,"updated":13.5,"status":"paused"}));
        assert_eq!((a.revision, b.revision), (1, 2));
        assert_eq!(a.digest, b.digest);
        assert_ne!(b.digest, c.digest);
        assert_eq!(Head::of(&json!({"id":"a"})), None, "a row needs its revision");
        let set = |heads: &[(&str, Head)]| -> Heads {
            heads.iter().map(|(id, head)| ((*id).to_owned(), *head)).collect()
        };
        assert!(!changed_beyond_revisions(&set(&[("a", a)]), &set(&[("a", b)])));
        assert!(changed_beyond_revisions(&set(&[("a", b)]), &set(&[("a", c)])));
        assert!(changed_beyond_revisions(&set(&[("a", a)]), &set(&[("a", a), ("b", a)])));
        assert!(changed_beyond_revisions(&set(&[("a", a)]), &set(&[("b", a)])));
    }
}
