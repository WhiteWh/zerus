//! Native-held conflict review; the only mutation runs on the existing worker.
use super::*;
use serde::Deserialize;
use zerus_client_core::swarm::conflicts as core;

pub(super) struct Selected {
    pub(super) id: String,
    conflict: String,
    raw: Arc<core::Review>,
    page: usize,
    chosen: Option<usize>,
    changed: bool,
}
impl Selected {
    pub(super) fn saved_status(&self, snapshot: &Value) -> Result<String, String> {
        if self.raw.matches(&core::Catalog::checked(snapshot)?) {
            return Err(connection::INVALID.into());
        }
        Ok(
            if snapshot["conflicts"].as_array().is_some_and(Vec::is_empty) {
                "Choice saved. No conflicts remain."
            } else {
                "Choice saved. Review the remaining conflicts."
            }
            .into(),
        )
    }
    pub(super) fn invocation(
        &self,
        snapshot: &Value,
        confirmed: bool,
    ) -> Result<Invocation, String> {
        self.raw.resolve(
            &core::Catalog::checked(snapshot)?,
            self.chosen.ok_or(core::CHOOSE)?,
            confirmed,
        )
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Open { conflict: String },
    View { review: String },
    Page { review: String, page: usize },
    Choose { review: String, choice: usize },
}
#[derive(Serialize)]
pub struct ReviewView {
    id: String,
    summary: core::Summary,
    explanation: String,
    choices: core::Page<core::Choice>,
    chosen: Option<usize>,
    selection: Option<core::Selection>,
}
impl Selected {
    fn view(&self, catalog: &core::Catalog<'_>) -> Result<ReviewView, String> {
        Ok(ReviewView {
            id: self.id.clone(),
            summary: catalog.summary_for(&self.conflict)?,
            explanation: self.raw.explanation(self.changed),
            choices: self.raw.choices(catalog, self.page)?,
            chosen: self.chosen,
            selection: self
                .chosen
                .map(|choice| self.raw.selection(catalog, choice))
                .transpose()?,
        })
    }
}

impl Organization {
    fn conflict_titles(&self, snapshot: &Value) -> impl Fn(&str, &str) -> Option<String> {
        let pump = self.0.pump.clone();
        let aliases: std::collections::BTreeMap<String, String> = snapshot["machines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                Some((
                    m["id"].as_str()?.to_owned(),
                    if m["local"] == true {
                        String::new()
                    } else {
                        m["connection"].as_str()?.to_owned()
                    },
                ))
            })
            .collect();
        move |machine, session| {
            if session.starts_with("archive\n") {
                return None;
            }
            let row = pump.row_named(aliases.get(machine)?, session)?;
            (!row.display_title.is_empty()).then_some(row.display_title)
        }
    }
    /// One bounded CPU review at a time. Capture the existing Arc under the hub
    /// lock, qualify/project outside it, then refuse if the source changed.
    fn conflict_capture(&self, id: &str) -> Result<(Arc<Value>, u64), String> {
        let mut hub = self.catalog_lock()?;
        let context = hub
            .catalog
            .as_ref()
            .filter(|c| c.id == id && c.visible)
            .ok_or(EXPIRED)?;
        if context.active() || context.ticket.is_some() {
            return Err(BUSY.into());
        }
        let snapshot = hub.sync.observed_value().ok_or(EXTERNAL_CHANGED)?;
        let revision = hub.sync.revision();
        hub.catalog.as_mut().unwrap().review_reading = true;
        Ok((snapshot, revision))
    }
    fn conflict_finish<'a>(
        &'a self,
        id: &str,
        snapshot: &Arc<Value>,
        revision: u64,
    ) -> Result<MutexGuard<'a, Hub>, String> {
        let mut hub = self.catalog_lock()?;
        let context = hub.catalog.as_mut().filter(|c| c.id == id).ok_or(EXPIRED)?;
        context.review_reading = false;
        if !context.visible || hub.stopped {
            return Err(EXPIRED.into());
        }
        if hub.sync.revision() != revision
            || !hub
                .sync
                .observed_value()
                .is_some_and(|now| Arc::ptr_eq(&now, snapshot))
        {
            return Err(core::CHANGED.into());
        }
        Ok(hub)
    }
    pub(in crate::organization) fn conflict_page(
        &self,
        id: &str,
        page: usize,
    ) -> Result<core::Page<core::Summary>, String> {
        let (snapshot, revision) = self.conflict_capture(id)?;
        let title = self.conflict_titles(&snapshot);
        let host = self
            .0
            .pump
            .machine(&ComputerId::local())
            .and_then(|m| m.host)
            .unwrap_or_default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            core::Catalog::checked(&snapshot)?
                .with_session_titles(&host, &title)?
                .page(page)
        }))
        .unwrap_or_else(|_| Err(STOPPED.into()));
        let _hub = self.conflict_finish(id, &snapshot, revision)?;
        result
    }
    pub(in crate::organization) fn conflict_review(
        &self,
        id: &str,
        query: Query,
    ) -> Result<ReviewView, String> {
        let (snapshot, revision) = self.conflict_capture(id)?;
        // The reading flag excludes another review/Resolve. Move the one
        // immutable selection out while doing bounded CPU work without the lock.
        let selected = {
            let mut hub = self.catalog_lock()?;
            hub.catalog.as_mut().unwrap().review.take()
        };
        let mut selected = selected;
        let title = self.conflict_titles(&snapshot);
        let host = self
            .0
            .pump
            .machine(&ComputerId::local())
            .and_then(|m| m.host)
            .unwrap_or_default();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let catalog = core::Catalog::checked(&snapshot)?.with_session_titles(&host, &title)?;
            if let Some(old) = selected.as_mut().filter(|old| !old.raw.matches(&catalog)) {
                old.chosen = None;
            }
            match &query {
                Query::Open { conflict } => {
                    if conflict.len() != 64 {
                        return Err(core::INVALID.into());
                    }
                    let changed = selected
                        .as_ref()
                        .is_some_and(|old| old.conflict == *conflict && !old.raw.matches(&catalog));
                    selected = Some(Selected {
                        id: uuid::Uuid::new_v4().to_string(),
                        raw: Arc::new(catalog.review(conflict)?),
                        conflict: conflict.clone(),
                        page: 0,
                        chosen: None,
                        changed,
                    });
                }
                Query::View { review }
                | Query::Page { review, .. }
                | Query::Choose { review, .. } => {
                    let current = selected
                        .as_mut()
                        .filter(|s| s.id == *review)
                        .ok_or(EXPIRED)?;
                    match &query {
                        Query::Page { page, .. } => {
                            current.page = *page;
                            current.chosen = None;
                        }
                        Query::Choose { choice, .. } => {
                            let values = current.raw.choices(&catalog, current.page)?;
                            if !values.rows.iter().any(|v| v.index == *choice) {
                                return Err(core::INVALID.into());
                            }
                            current.chosen = Some(*choice);
                        }
                        _ => (),
                    }
                }
            }
            selected.as_ref().ok_or(EXPIRED)?.view(&catalog)
        }))
        .unwrap_or_else(|_| Err(STOPPED.into()));
        {
            let mut hub = self.catalog_lock()?;
            let context = hub.catalog.as_mut().filter(|c| c.id == id).ok_or(EXPIRED)?;
            if context.visible {
                context.review = selected;
            }
        }
        let _hub = self.conflict_finish(id, &snapshot, revision)?;
        result
    }
}

#[tauri::command]
pub async fn project_swarm_conflicts(
    window: tauri::Webview,
    state: State<'_, Organization>,
    id: String,
    page: usize,
) -> Result<core::Page<core::Summary>, String> {
    main(&window)?;
    let org = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || org.conflict_page(&id, page))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn project_swarm_conflict_review(
    window: tauri::Webview,
    state: State<'_, Organization>,
    id: String,
    query: Query,
) -> Result<ReviewView, String> {
    main(&window)?;
    let org = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || org.conflict_review(&id, query))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn project_swarm_resolve(
    window: tauri::Webview,
    state: State<'_, Organization>,
    id: String,
    review: String,
    confirmed: bool,
) -> Result<View, String> {
    main(&window)?;
    command(
        state.inner().clone(),
        Some(id),
        Some(Action::Resolve { review, confirmed }),
        false,
    )
    .await
}
