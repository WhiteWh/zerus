//! Immutable observed generations with a separate read-only listing view.
use super::*;
use serde::{Deserialize, Serialize};
use zerus_client_core::source;
pub(super) const TERMINAL_CANDIDATES: &str = "SELECT substr(s.source,1,257),CASE WHEN length(CAST(s.seed AS BLOB))<=262144 THEN s.seed ELSE NULL END,substr(s.stamp,1,1025) FROM sources AS locator INDEXED BY sources_name CROSS JOIN snapshots AS s WHERE locator.tombstone=0 AND json_extract(locator.seed,'$.record.name')=?1 AND s.generation=?2 AND s.source=locator.source LIMIT 3";
const ROWS: usize = 128;
const BYTES: usize = 512 << 10;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Cursor {
    epoch: String,
    generation: i64,
    high_water: i64,
    after: String,
}
pub(super) struct Page {
    pub rows: Vec<(String, Value)>,
    pub next: Option<Cursor>,
    pub fresh: bool,
}
impl Store {
    pub fn page(&self, cursor: Option<&Cursor>) -> anyhow::Result<Page> {
        self.page_limit(cursor, ROWS)
    }
    pub fn page_limit(&self, cursor: Option<&Cursor>, limit: usize) -> anyhow::Result<Page> {
        anyhow::ensure!((1..=ROWS).contains(&limit), "Invalid display page size");
        self.verify()?;
        let (epoch, active, ready): (String, i64, bool) = self.db.query_row(
            "SELECT epoch,generation,ready FROM meta WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let generation = cursor.map_or(active, |c| c.generation);
        anyhow::ensure!(generation > 0, "Listing metadata is still rebuilding");
        if let Some(c) = cursor {
            anyhow::ensure!(
                c.epoch == epoch && c.after.len() <= 256,
                "Listing metadata cursor is stale"
            );
        }
        let (dirs, gap, high_water): (String, String, i64) = self.db.query_row(
            "SELECT dirs,gap,high_water FROM generations WHERE generation=?1 AND complete=1",
            [generation],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if let Some(c) = cursor {
            anyhow::ensure!(c.high_water == high_water, "Display snapshot cut changed");
        }
        let mut fresh = ready
            && generation == active
            && repair::directories(self)? == dirs
            && repair::gap_stamp(self)? == gap;
        let after = cursor.map_or("", |c| c.after.as_str());
        let mut statement=self.db.prepare_cached("SELECT substr(source,1,257),CASE WHEN length(CAST(seed AS BLOB))<=262144 THEN seed ELSE NULL END,substr(stamp,1,1025) FROM snapshots WHERE generation=?1 AND source>?2 ORDER BY source LIMIT ?3")?;
        let mut query = statement.query(params![generation, after, limit])?;
        let mut rows = Vec::new();
        let mut bytes = 0;
        let until = Instant::now() + Duration::from_millis(250);
        while let Some(row) = query.next()? {
            anyhow::ensure!(
                Instant::now() < until,
                "Display page verification timed out"
            );
            let source: String = row.get(0)?;
            let seed: String = row.get(1)?;
            let stamp: String = row.get(2)?;
            anyhow::ensure!(
                source.len() <= 256 && seed.len() <= model::SEED_BYTES && stamp.len() <= 1024,
                "Invalid display page row"
            );
            if bytes + source.len() + seed.len() + stamp.len() > BYTES {
                break;
            }
            bytes += source.len() + seed.len() + stamp.len();
            let key = Key(source.clone());
            let mut value = Seed::decode(&key, &seed)?;
            value["native_stamp"] = json!(stamp);
            fresh &= self.source_stamp(&key).ok().flatten().as_deref() == Some(stamp.as_str());
            rows.push((source, value));
        }
        anyhow::ensure!(
            rows.len() <= ROWS && bytes <= BYTES,
            "Display page exceeds its bound"
        );
        let last = rows.last().map(|r| r.0.as_str()).unwrap_or(after);
        let more: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM snapshots WHERE generation=?1 AND source>?2)",
            params![generation, last],
            |r| r.get(0),
        )?;
        anyhow::ensure!(!more || !rows.is_empty(), "Display page cannot advance");
        let next = more.then(|| Cursor {
            epoch,
            generation,
            high_water,
            after: last.into(),
        });
        self.verify()?;
        Ok(Page { rows, next, fresh })
    }
}

/// The public reader surface intentionally exposes no reservation, settlement,
/// recovery or canonical effect method.
pub(super) struct Reader(Store, std::cell::Cell<usize>);
impl Reader {
    pub fn verify_source(&self, source: &str, stamp: &str) -> anyhow::Result<()> {
        let key = Key(source.into());
        key.validate()?;
        anyhow::ensure!(
            stamp.len() <= 1024 && self.0.source_stamp(&key)?.as_deref() == Some(stamp),
            source!("Public listing source changed before publication")
        );
        Ok(())
    }
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        Ok(Self(Store::read_listing(root)?, std::cell::Cell::new(BYTES)))
    }
    pub fn page(&self, cursor: Option<&Cursor>) -> anyhow::Result<Page> {
        self.page_limit(cursor, ROWS)
    }
    pub fn page_limit(&self, cursor: Option<&Cursor>, limit: usize) -> anyhow::Result<Page> {
        let mut page = self.0.page_limit(cursor, limit)?;
        // The retained RO transaction pins generation data, not current readiness.
        // Read current metadata separately without taking the publisher gate.
        let check = || -> anyhow::Result<bool> {
            let pinned: (String, i64, i64) = self.0.db.query_row(
                "SELECT epoch,generation,writes FROM meta WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let current = Store::read(&self.0.root.path)?;
            let (epoch, generation, writes, ready): (String, i64, i64, bool) =
                current.db.query_row(
                    "SELECT epoch,generation,writes,ready FROM meta WHERE id=1",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
            Ok(ready && pinned == (epoch, generation, writes))
        };
        page.fresh &= check().unwrap_or(false);
        Ok(page)
    }
}

impl Reader {
    /// Only for archive display. The caller still verifies the complete
    /// generation before publishing its listing.
    pub fn has_label(&self, name: &str) -> anyhow::Result<bool> {
        let mut query = self.0.db.prepare_cached(
            "SELECT EXISTS(
                SELECT 1 FROM sources AS locator INDEXED BY sources_name
                CROSS JOIN snapshots AS s
                WHERE locator.tombstone=0 AND json_extract(locator.seed,'$.record.name')=?1
                  AND s.generation=?2 AND s.source=locator.source
            )",
        )?;
        Ok(query.query_row(params![name, self.generation()?], |row| row.get(0))?)
    }

    pub fn parent(&self, link: &Value) -> anyhow::Result<Option<(String, Value)>> {
        let generation: i64 =
            self.0
                .db
                .query_row("SELECT generation FROM meta WHERE id=1", [], |r| r.get(0))?;
        let (sql, key) = if !string(link, "session_id").is_empty() {
            (
                "SELECT substr(source,1,257),CASE WHEN length(CAST(seed AS BLOB))<=262144 THEN seed ELSE NULL END,substr(stamp,1,1025) FROM snapshots INDEXED BY snapshots_session WHERE generation=?1 AND json_extract(seed,'$.record.session_id')=?2 LIMIT 3",
                string(link, "session_id"),
            )
        } else {
            (
                "SELECT substr(source,1,257),CASE WHEN length(CAST(seed AS BLOB))<=262144 THEN seed ELSE NULL END,substr(stamp,1,1025) FROM snapshots INDEXED BY snapshots_conversation WHERE generation=?1 AND json_extract(seed,'$.record.conversation_id')=?2 LIMIT 3",
                string(link, "conversation_id"),
            )
        };
        if key.is_empty() {
            return Ok(None);
        }
        let mut query = self.0.db.prepare_cached(sql)?;
        let found = query
            .query_map(params![generation, key], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        anyhow::ensure!(
            found.len() <= 1,
            "Parent display identity is ambiguous; refresh the listing"
        );
        let Some((source, seed, stamp)) = found.into_iter().next() else {
            return Ok(None);
        };
        self.point_row(source, seed, stamp).map(Some)
    }

    /// A label is only a bounded locator. Read the candidate from this exact
    /// immutable generation, never the mutable locator's seed or stamp.
    pub fn terminal_candidates(&self, name: &str) -> anyhow::Result<Vec<(String, Value)>> {
        anyhow::ensure!(name.len() <= 2048, "Native display label exceeds its bound");
        let generation = self.generation()?;
        let mut query = self.0.db.prepare_cached(TERMINAL_CANDIDATES)?;
        let found = query
            .query_map(params![name, generation], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        anyhow::ensure!(
            found.len() < 3,
            "Native display candidates are ambiguous; use a smaller listing after recovery"
        );
        found
            .into_iter()
            .map(|(source, seed, stamp)| self.point_row(source, seed, stamp))
            .collect()
    }

    fn point_row(
        &self,
        source: String,
        seed: String,
        stamp: String,
    ) -> anyhow::Result<(String, Value)> {
        let bytes = source.len() + seed.len() + stamp.len();
        anyhow::ensure!(
            source.len() <= 256 && seed.len() <= model::SEED_BYTES && stamp.len() <= 1024,
            "Invalid parent display row"
        );
        anyhow::ensure!(
            bytes <= self.1.get(),
            "Parent display query byte budget exceeded"
        );
        self.1.set(self.1.get() - bytes);
        let key = Key(source);
        anyhow::ensure!(
            self.0.source_stamp(&key)?.as_deref() == Some(stamp.as_str()),
            "Parent display source changed"
        );
        let mut value = Seed::decode(&key, &seed)?;
        value["native_stamp"] = json!(stamp);
        Ok((key.0, value))
    }
    pub fn generation(&self) -> anyhow::Result<i64> {
        Ok(self
            .0
            .db
            .query_row("SELECT generation FROM meta WHERE id=1", [], |r| r.get(0))?)
    }
}

impl Cursor {
    pub fn encode(&self) -> anyhow::Result<String> {
        use base64::Engine;
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(self)?))
    }
    pub fn decode(value: &str) -> anyhow::Result<Self> {
        use base64::Engine;
        anyhow::ensure!(value.len() <= 2048, "Display cursor exceeds its bound");
        Ok(serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value)?,
        )?)
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
}
