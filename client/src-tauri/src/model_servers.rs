//! Task 67: the Model servers tab and Settings > Model servers. Every call
//! is client/core's guarded `hgs model-server ...` on this computer; the last
//! list read is kept here so Save and Delete send the whole list as Qt does.
use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};
use tauri::State;
use zerus_client_core::default_hgs;
use zerus_client_core::model_servers::{
    self as core, Form, Logs, Op, Server, Status, TestResult, UpPoll, UpStart,
};
use zerus_client_core::text::Text;

pub struct ModelServers {
    replay: bool,
    list: Mutex<Vec<Server>>,
}

impl ModelServers {
    pub fn new(replay: bool) -> Self {
        Self {
            replay,
            list: Mutex::new(Vec::new()),
        }
    }
    fn stored(&self) -> Vec<Server> {
        self.list
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn keep(&self, servers: Vec<Server>) -> Vec<Server> {
        *self.list.lock().unwrap_or_else(PoisonError::into_inner) = servers.clone();
        servers
    }
    fn server(&self, name: &str) -> Server {
        self.stored()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_default()
    }
}

/// Runs one operation off the UI thread; a replay runs no hgs.
async fn call(replay: bool, op: Op) -> Result<serde_json::Value, String> {
    if replay {
        return Err("Model servers need hgs; a replay has none.".into());
    }
    tauri::async_runtime::spawn_blocking(move || core::run(&default_hgs(), &op))
        .await
        .map_err(|error| error.to_string())?
}

/// The list; an hgs without model servers lists none (no tab, as in Qt).
#[tauri::command]
pub async fn model_servers_list(servers: State<'_, ModelServers>) -> Result<Vec<Server>, String> {
    let listed = call(servers.replay, Op::List)
        .await
        .map(|value| core::parse_list(&value))
        .unwrap_or_default();
    Ok(servers.keep(listed))
}

#[tauri::command]
pub async fn model_server_status(
    servers: State<'_, ModelServers>,
    server: String,
) -> Result<Status, String> {
    let reply = call(
        servers.replay,
        Op::Status {
            server: server.clone(),
        },
    )
    .await;
    Ok(core::parse_status(
        &servers.server(&server),
        reply.as_ref().map_err(String::as_str),
    ))
}

#[tauri::command]
pub async fn model_server_up(
    servers: State<'_, ModelServers>,
    server: String,
    model: String,
) -> Result<UpStart, String> {
    let reply = call(
        servers.replay,
        Op::Up {
            server: server.clone(),
            model: model.clone(),
        },
    )
    .await;
    Ok(core::up_started(
        &server,
        &model,
        reply.as_ref().map_err(String::as_str),
    ))
}

/// One read of a detached Up's run; `expired` once its time limit passed.
#[tauri::command]
pub async fn model_server_up_result(
    servers: State<'_, ModelServers>,
    server: String,
    model: String,
    id: String,
    expired: bool,
) -> Result<UpPoll, String> {
    let reply = call(servers.replay, Op::UpResult { id: id.clone() }).await;
    Ok(core::up_poll(
        &server,
        &model,
        &id,
        reply.as_ref().map_err(String::as_str),
        expired,
    ))
}

#[tauri::command]
pub async fn model_server_sessions(
    servers: State<'_, ModelServers>,
    server: String,
) -> Result<BTreeMap<String, Vec<String>>, Text> {
    let reply = call(servers.replay, Op::Sessions { server }).await;
    core::sessions_by_model(reply.as_ref().map_err(String::as_str))
}

/// One confirmed Down: its failure, if any.
#[tauri::command]
pub async fn model_server_down(
    servers: State<'_, ModelServers>,
    server: String,
    model: String,
) -> Result<Option<Text>, String> {
    let reply = call(servers.replay, Op::Down { server, model }).await;
    Ok(core::down_failure(reply.as_ref().map_err(String::as_str)))
}

/// model-catalog/2: one confirmed variant change (a running service restarts
/// with `restart`); its failure, if any.
#[tauri::command]
pub async fn model_server_variant(
    servers: State<'_, ModelServers>,
    server: String,
    model: String,
    variant: String,
    restart: bool,
) -> Result<Option<Text>, String> {
    let reply = call(servers.replay, Op::Variant { server, model, variant, restart }).await;
    Ok(core::down_failure(reply.as_ref().map_err(String::as_str)))
}

#[tauri::command]
pub async fn model_server_logs(
    servers: State<'_, ModelServers>,
    server: String,
    model: String,
) -> Result<Logs, Text> {
    let reply = call(servers.replay, Op::Logs { server, model }).await;
    core::logs(reply.as_ref().map_err(String::as_str))
}

async fn save(servers: &ModelServers, list: Vec<serde_json::Value>) -> Result<Vec<Server>, Text> {
    let reply = call(servers.replay, Op::Save { servers: list }).await;
    if let Some(why) = core::save_refusal(reply.as_ref().map_err(String::as_str)) {
        return Err(why);
    }
    Ok(servers.keep(core::parse_list(&reply.unwrap_or_default())))
}

/// Save: the whole list, this form in its server's place.
#[tauri::command]
pub async fn model_server_save(
    servers: State<'_, ModelServers>,
    form: Form,
) -> Result<Vec<Server>, Text> {
    let list = core::saved_list(&servers.stored(), &form);
    save(&servers, list).await
}

/// Delete server (confirmed by the owner first): every other server.
#[tauri::command]
pub async fn model_server_delete(
    servers: State<'_, ModelServers>,
    name: String,
) -> Result<Vec<Server>, Text> {
    let list = core::without(&servers.stored(), &name);
    save(&servers, list).await
}

/// Test API (`api`) or Test status (`status`): once, nothing saved.
#[tauri::command]
pub async fn model_server_test(
    servers: State<'_, ModelServers>,
    form: Form,
    what: String,
) -> Result<TestResult, String> {
    let server = form.server(&servers.stored(), what == "status");
    let reply = call(servers.replay, Op::Test { server }).await;
    Ok(core::test_result(
        &what,
        &form.base_url,
        reply.as_ref().map_err(String::as_str),
    ))
}

/// Test logs: the saved server's logs once, with a model its status listed.
#[tauri::command]
pub async fn model_server_test_logs(
    servers: State<'_, ModelServers>,
    form: Form,
    model: String,
) -> Result<TestResult, String> {
    let stored = servers.server(&form.was);
    let saved = (!form.was.is_empty() && stored.name == form.was).then_some(&stored);
    let ready = core::logs_test(saved, &form.logs, &model, None);
    if ready.text.english() != "Not read" {
        return Ok(ready);
    }
    let reply = call(
        servers.replay,
        Op::Logs {
            server: form.was.clone(),
            model: model.clone(),
        },
    )
    .await;
    Ok(core::logs_test(
        saved,
        &form.logs,
        &model,
        Some(reply.as_ref().map_err(String::as_str)),
    ))
}

/// New session on a model server: hgs starts it as Qt's launch does.
#[tauri::command]
pub async fn model_server_session(
    servers: State<'_, ModelServers>,
    agent: String,
    folder: String,
    tag: String,
    server: String,
    model: String,
) -> Result<(), String> {
    if servers.replay {
        return Err("Model servers need hgs; a replay has none.".into());
    }
    let launch = uuid::Uuid::new_v4().to_string();
    let call = core::session_launch(&agent, &folder, &tag, &server, &model, &launch)?;
    tauri::async_runtime::spawn_blocking(move || core::launch(&default_hgs(), &call))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kept_list_is_what_save_and_delete_send() {
        let servers = ModelServers::new(false);
        let listed = core::parse_list(
            &serde_json::json!({"servers": [{"name": "lab", "base_url": "http://x", "kind": "openai"}]}),
        );
        servers.keep(listed);
        assert_eq!(servers.server("lab").name, "lab");
        assert_eq!(servers.server("gone"), Server::default());
        assert_eq!(core::without(&servers.stored(), "lab").len(), 0);
    }
}
