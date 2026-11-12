//! A one-statement, connection-local exemption for a validated original pin.
use super::*;
use rusqlite::config::DbConfig;

const TRIGGERS: DbConfig = DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER;

pub(super) struct ProvenPin {
    _validated: (),
}
impl ProvenPin {
    pub(super) fn read(
        source: Source,
        path: &Path,
        requested: &Binding,
        stamp: &str,
    ) -> Option<Self> {
        let value = read_original(path).ok()?;
        let proof = Binding::parse(source, &value, path.file_name()?.to_str()?).ok()?;
        (proof.id == requested.id
            && proof.fingerprint == requested.fingerprint
            && proof.name == requested.name
            && proof.run == requested.run
            && proof.conversation == requested.conversation
            && proof.agent == requested.agent
            && proof.first == requested.first
            && proof.at == requested.at
            && proof.files == requested.files
            && file_stamp(path).ok().as_deref() == Some(stamp))
        .then_some(Self { _validated: () })
    }
    pub(super) fn update(
        self,
        db: &Connection,
        write: impl FnOnce() -> rusqlite::Result<usize>,
    ) -> Result<usize> {
        if !sql(db.db_config(TRIGGERS))? {
            return Err("Receipt index guards are unavailable".into());
        }
        let mut guard = TriggerGuard { db, active: true };
        sql(db.set_db_config(TRIGGERS, false))?;
        let result = write();
        guard.restore()?;
        sql(result)
    }
}
struct TriggerGuard<'a> {
    db: &'a Connection,
    active: bool,
}
impl TriggerGuard<'_> {
    fn restore(&mut self) -> Result<()> {
        if !sql(self.db.set_db_config(TRIGGERS, true))? {
            return Err("Receipt index guards are unavailable".into());
        }
        self.active = false;
        Ok(())
    }
}
impl Drop for TriggerGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.db.set_db_config(TRIGGERS, true);
        }
    }
}
