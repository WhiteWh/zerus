//! Endpoint-owned workspace enrollment and invitation control. Secrets are
//! written only to private local records; reports select public fields.
use crate::{
    error::{ConnectorError, Result},
    http::Relay,
    private_state::Store,
    workspace_cli,
};
use http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use zerus_mobile_crypto::{
    self as crypto, EnrollmentCandidate, Identity, IdentityRecord, Invitation, MembershipAction,
    MembershipState, MembershipStatement, Role, SignedEnrollment, SignedMembership,
    SignedMembershipSnapshot,
};

const ACTIVE: &str = "active.json";
const PENDING: &str = "pending.json";
const INVITATIONS: &str = "invitations.json";
const MAX_INVITATIONS: usize = 64;
fn false_flag(value: &bool) -> bool {
    !*value
}
const PAGE: usize = 100;

/// Never Debug: bearer and private keys are intentionally absent from reports.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub server_url: String,
    pub workspace_name: String,
    pub computer_name: String,
    pub node_token: String,
    pub identity: IdentityRecord,
    pub membership: MembershipState,
    pub historical_heads: BTreeMap<u64, String>,
    pub state_dir: PathBuf,
}
impl Profile {
    fn validate_record(&self) -> Result<()> {
        let identity = cc(Identity::from_record(&self.identity))?;
        cc(self.membership.validate())?;
        if identity.public().role != Role::Computer
            || (self.membership.members.get(&identity.public().device_id)
                != Some(identity.public())
                && !self.retired())
            || self.historical_heads.get(&self.membership.revision) != Some(&self.membership.head)
            || !self.state_dir.is_absolute()
            || self.node_token.is_empty()
        {
            return Err(fail("Workspace identity is unavailable or retired"));
        }
        if self.historical_heads.len() > 8192 {
            return Err(fail("Workspace membership history exceeds its bound"));
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<()> {
        self.validate_record()?;
        if self.retired() {
            return Err(fail("Workspace identity is unavailable or retired"));
        }
        Ok(())
    }
    fn retired(&self) -> bool {
        self.membership
            .revoked_device_ids
            .contains(&self.identity.device.device_id)
    }
    fn report(&self) -> Value {
        json!({"state":if self.retired(){"retired"}else{"active"},"encrypted":true,"server_url":self.server_url,"workspace_id":self.membership.workspace_id,"workspace_name":self.workspace_name,"computer_name":self.computer_name,"node_id":self.identity.device.device_id,"membership_revision":self.membership.revision,"membership_head":self.membership.head})
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    version: u8,
    action: String,
    expected_revision: String,
    server_url: String,
    workspace_name: String,
    computer_name: String,
    node_token: String,
    identity: IdentityRecord,
    genesis: Option<SignedMembership>,
    candidate: Option<EnrollmentCandidate>,
    invitation: Option<Invitation>,
    profile: Option<Profile>,
    previous_profile: Option<Profile>,
    retirement: Option<SignedMembership>,
    #[serde(default)]
    legacy_retirement_request: Option<String>,
    #[serde(default)]
    legacy_retirement_status: Option<String>,
    #[serde(default, skip_serializing_if = "false_flag")]
    legacy_service_handover: bool,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RetainedInvitation {
    invitation: Invitation,
    bootstrap: SignedMembershipSnapshot,
    approval: Option<SignedEnrollment>,
}

fn fail(message: &'static str) -> ConnectorError {
    ConnectorError::new(message)
}
fn cc<T>(result: crypto::Result<T>) -> Result<T> {
    result.map_err(|_| fail("Workspace authentication failed; private content was not accepted"))
}
fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| fail("Private workspace state is unavailable or in use"))
}
fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|_| fail("Invalid workspace protocol response"))
}
fn decode_record<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|_| fail("Private workspace state is invalid"))
}
fn save<T: Serialize>(store: &Store, name: &str, value: &T) -> Result<()> {
    let expected = Store::revision(io(store.read(name))?.as_deref());
    let bytes = serde_json::to_vec(value)
        .map_err(|_| fail("Private workspace state could not be encoded"))?;
    io(store.replace(name, &expected, &bytes))?;
    Ok(())
}
fn stored<T: serde::de::DeserializeOwned>(store: &Store, name: &str) -> Result<Option<T>> {
    io(store.read(name))?
        .map(|bytes| decode_record(&bytes))
        .transpose()
}
fn ensure_advance_fits(profile: &Profile, change: &SignedMembership) -> Result<()> {
    let mut membership = profile.membership.clone();
    cc(membership.apply_next(change))?;
    let mut heads = profile.historical_heads.clone();
    heads.insert(membership.revision, membership.head.clone());
    #[derive(Serialize)]
    struct Projection<'a> {
        server_url: &'a str,
        workspace_name: &'a str,
        computer_name: &'a str,
        node_token: &'a str,
        identity: &'a IdentityRecord,
        membership: &'a MembershipState,
        historical_heads: &'a BTreeMap<u64, String>,
        state_dir: &'a Path,
    }
    let bytes = serde_json::to_vec(&Projection {
        server_url: &profile.server_url,
        workspace_name: &profile.workspace_name,
        computer_name: &profile.computer_name,
        node_token: &profile.node_token,
        identity: &profile.identity,
        membership: &membership,
        historical_heads: &heads,
        state_dir: &profile.state_dir,
    })
    .map_err(|_| fail("Private workspace state could not be encoded"))?;
    if bytes.len() > crate::private_state::MAX_STATE_BYTES {
        return Err(fail(
            "Workspace membership exceeds the local private state bound",
        ));
    }
    Ok(())
}
fn text(input: &Value, name: &str, default: &str) -> Result<String> {
    let value = input
        .get(name)
        .map_or(Some(default), Value::as_str)
        .ok_or_else(|| fail("Invalid workspace input"))?;
    if value.len() > 1024 || value.chars().any(char::is_control) {
        return Err(fail("Invalid workspace input"));
    }
    Ok(value.to_owned())
}
fn private_directory(config: &Path) -> Result<PathBuf> {
    let path = config
        .parent()
        .ok_or_else(|| fail("Invalid connector configuration path"))?
        .join("workspace-private");
    if !path.is_absolute() {
        return Err(fail("Connector configuration path must be absolute"));
    }
    Ok(path)
}
fn validate_marker(config: &Value, private_dir: &Path) -> Result<()> {
    let marker = config
        .get("encrypted_workspace")
        .and_then(Value::as_object)
        .filter(|m| m.len() == 2)
        .ok_or_else(|| fail("Invalid encrypted workspace configuration"))?;
    if marker.get("version").and_then(Value::as_u64) != Some(1)
        || marker.get("state_dir").and_then(Value::as_str) != private_dir.to_str()
        || config.get("node_token").is_some()
    {
        return Err(fail("Invalid encrypted workspace configuration"));
    }
    Ok(())
}
pub(crate) fn validate_config_profile_binding(
    config: &Value,
    private_dir: &Path,
    profile: &Profile,
) -> Result<()> {
    validate_marker(config, private_dir)?;
    profile.validate_record()?;
    if config["server_url"].as_str() != Some(&profile.server_url)
        || config["state_dir"] != json!(profile.state_dir)
        || profile.state_dir
            != private_dir.join(format!("receipts-{}", profile.identity.device.device_id))
    {
        return Err(fail(
            "Encrypted workspace configuration does not match its private identity",
        ));
    }
    Ok(())
}
fn pending_activation_matches(config: &Value, revision: &str, private_dir: &Path) -> Result<bool> {
    let Some(bytes) = workspace_cli::read_private(private_dir, PENDING)? else {
        return Ok(false);
    };
    let Some(pending) = decode_record::<Option<Pending>>(&bytes)? else {
        return Ok(false);
    };
    if pending.version != 1 || pending.expected_revision != revision || pending.profile.is_none() {
        return Ok(false);
    }
    Ok(pending.previous_profile.as_ref().is_some_and(|profile| {
        validate_config_profile_binding(config, private_dir, profile).is_ok()
    }))
}
fn service_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| fail("Secure randomness unavailable"))?;
    Ok(crypto::encode(&bytes))
}
pub fn relay(profile: &Profile, allow_insecure_test: bool) -> Result<Relay> {
    relay_for(
        &profile.server_url,
        &profile.node_token,
        allow_insecure_test,
    )
}
fn relay_for(server: &str, token: &str, allow_insecure_test: bool) -> Result<Relay> {
    let server = crate::url::checked_url(server, allow_insecure_test)?;
    let transport = crate::url::Transport::new(&server)?;
    let roots = if transport.secure {
        crate::http::system_roots()
    } else {
        None
    };
    Ok(Relay::new(&transport, token, roots))
}
async fn call(relay: &Relay, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
    let body = body
        .map(|v| serde_json::to_vec(&v).map_err(|_| fail("Invalid workspace request")))
        .transpose()?;
    relay
        .call(method, path, body, 2 * 1024 * 1024)
        .await
        .map_err(|e| match e.status() {
            Some(409) => fail("Workspace changed; reload before continuing"),
            Some(401 | 403) => fail("Workspace authorization was refused"),
            _ => fail("Workspace relay is unavailable; the exact pending operation is retained"),
        })?
        .ok_or_else(|| fail("Workspace relay returned no result"))
}
pub fn load_active(private_dir: &Path) -> Result<Profile> {
    let profile = load_record(private_dir)?;
    profile.validate()?;
    Ok(profile)
}
/// Inspect only the endpoint-owned, validated record. Relay HTTP status and
/// bearer refusal never establish retirement. A true result allows graceful
/// local shutdown; it grants no request, enrollment or recovery authority.
pub fn is_verified_retired(private_dir: &Path) -> Result<bool> {
    Ok(load_record(private_dir)?.retired())
}
pub(crate) fn load_record(private_dir: &Path) -> Result<Profile> {
    let bytes = workspace_cli::read_private(private_dir, ACTIVE)?
        .ok_or_else(|| fail("No encrypted workspace is configured"))?;
    let profile: Profile = decode_record(&bytes)?;
    profile.validate_record()?;
    Ok(profile)
}
/// Read the same validated identity through an already-held mutation Store.
#[cfg(target_os = "linux")]
pub(crate) fn load_record_from_store(store: &Store) -> Result<Profile> {
    let bytes =
        io(store.read(ACTIVE))?.ok_or_else(|| fail("No encrypted workspace is configured"))?;
    let profile: Profile = decode_record(&bytes)?;
    profile.validate_record()?;
    Ok(profile)
}
/// Optional switch evidence, never authority for Start. Archive recovery is
/// limited to the exact saved pending destination after ACTIVE publication.
#[cfg(target_os = "linux")]
pub(crate) fn retired_switch_profile_from_store(
    store: &Store,
    config: &Value,
    config_path: &Path,
    revision: &str,
    previous_node: &str,
    next_node: &str,
    archive_allowed: bool,
) -> Result<Option<Profile>> {
    let private = private_directory(config_path)?;
    let current = load_record_from_store(store)?;
    if current.identity.device.device_id == previous_node
        && current.retired()
        && validate_config_profile_binding(config, &private, &current).is_ok()
    {
        return Ok(Some(current));
    }
    if !archive_allowed {
        return Ok(None);
    }
    let Some(pending) = stored::<Option<Pending>>(store, PENDING)?.flatten() else {
        return Ok(None);
    };
    if pending.version != 1
        || !matches!(pending.action.as_str(), "switch-create" | "switch-join")
        || pending.expected_revision != revision
        || pending.identity.device.device_id != next_node
    {
        return Ok(None);
    }
    let Some(destination) = pending.profile.as_ref() else {
        return Ok(None);
    };
    destination.validate()?;
    current.validate()?;
    cc(Identity::from_record(&pending.identity))?;
    if destination.identity.device != pending.identity.device
        || destination.server_url != pending.server_url
        || destination.node_token != pending.node_token
        || destination.state_dir != private.join(format!("receipts-{next_node}"))
        || current.identity.device != destination.identity.device
        || current.server_url != destination.server_url
        || current.workspace_name != destination.workspace_name
        || current.computer_name != destination.computer_name
        || current.node_token != destination.node_token
        || current.state_dir != destination.state_dir
        || current.membership != destination.membership
        || current.historical_heads != destination.historical_heads
    {
        return Ok(None);
    }
    let Some(previous) = pending.previous_profile.as_ref() else {
        return Ok(None);
    };
    validate_config_profile_binding(config, &private, previous)?;
    if previous.identity.device.device_id != previous_node {
        return Ok(None);
    }
    let Some(archived) = stored::<Profile>(store, &format!("retired-{previous_node}.json"))? else {
        return Ok(None);
    };
    validate_config_profile_binding(config, &private, &archived)?;
    if !archived.retired()
        || archived.identity.device != previous.identity.device
        || archived.node_token != previous.node_token
        || archived.membership.workspace_id != previous.membership.workspace_id
        || archived.membership.revision < previous.membership.revision
        || archived.historical_heads.get(&previous.membership.revision)
            != Some(&previous.membership.head)
        || previous
            .historical_heads
            .iter()
            .any(|(revision, head)| archived.historical_heads.get(revision) != Some(head))
    {
        return Ok(None);
    }
    Ok(Some(archived))
}
pub fn public_status(config_path: &Path) -> Result<Value> {
    let (config, revision) = workspace_cli::read_config(config_path)?;
    let private_dir = private_directory(config_path)?;
    let mut result = if config.get("encrypted_workspace").is_some() {
        validate_marker(&config, &private_dir)?;
        let profile = load_record(&private_dir)?;
        if validate_config_profile_binding(&config, &private_dir, &profile).is_err() {
            if !pending_activation_matches(&config, &revision, &private_dir)? {
                return Err(fail(
                    "Encrypted workspace configuration does not match its private identity",
                ));
            }
            json!({"state":"pending-activation","encrypted":true})
        } else {
            let mut report = profile.report();
            let lifecycle = crate::workspace_lifecycle::status(&profile.state_dir)?;
            report["connector_state"] = lifecycle["state"].clone();
            report["running"] = lifecycle["running"].clone();
            report["generation"] = lifecycle["generation"].clone();
            report
        }
    } else {
        let mut report = json!({"state":if config.get("node_token").is_some(){"legacy"}else{"unconfigured"},"encrypted":false});
        if let Some(server) = config
            .get("server_url")
            .and_then(Value::as_str)
            .and_then(|server| crate::url::checked_url(server, false).ok())
        {
            report["server_url"] = json!(server);
        }
        report
    };
    result["config_revision"] = json!(revision);
    if private_dir.exists() {
        if let Some(bytes) = workspace_cli::read_private(&private_dir, PENDING)? {
            let pending: Option<Pending> = decode_record(&bytes)?;
            if let Some(pending) = pending {
                result["pending"] = json!({"action":pending.action,"node_id":pending.identity.device.device_id,"state":if pending.legacy_retirement_status.as_deref()==Some("unconfirmed"){"legacy-retirement-unconfirmed"}else if pending.profile.is_some(){"ready"}else{"pending"}});
                if let Some(migration) = crate::workspace_service::status(
                    &private_dir,
                    &revision,
                    &pending.identity.device.device_id,
                )? {
                    result["pending"]["state"] = json!(if migration["phase"] == "draining" {
                        "migration-draining"
                    } else {
                        "migration-blocked"
                    });
                    result["migration"] = migration;
                }
            }
        }
    }
    Ok(result)
}

async fn synchronize(store: &Store, profile: &mut Profile, relay: &Relay) -> Result<()> {
    synchronize_record(store, profile, relay).await?;
    profile.validate()
}
fn publish_membership(
    store: &Store,
    profile: &mut Profile,
    membership: MembershipState,
    heads: BTreeMap<u64, String>,
) -> Result<()> {
    let previous_membership = std::mem::replace(&mut profile.membership, membership);
    let previous_heads = std::mem::replace(&mut profile.historical_heads, heads);
    let published = profile
        .validate_record()
        .and_then(|()| save(store, ACTIVE, profile));
    if published.is_err() {
        let attempted_membership = std::mem::replace(&mut profile.membership, previous_membership);
        let attempted_heads = std::mem::replace(&mut profile.historical_heads, previous_heads);
        // A failed replacement may have committed before its directory sync
        // failed. Reload it, but retain only the exact authenticated page we
        // attempted to publish. If reloading also fails, keep the prior pin;
        // activation must never save uncommitted progress into PENDING.
        if let Ok(Some(current)) = stored::<Profile>(store, ACTIVE) {
            if current.validate_record().is_ok()
                && current.identity.device == profile.identity.device
                && current.server_url == profile.server_url
                && current.node_token == profile.node_token
                && current.state_dir == profile.state_dir
                && current.membership == attempted_membership
                && current.historical_heads == attempted_heads
            {
                profile.membership = current.membership;
                profile.historical_heads = current.historical_heads;
            }
        }
    }
    published
}
/// Activation may consume a verified self-tombstone. Ordinary authorization
/// continues through `synchronize`, which requires the identity to be active.
async fn synchronize_record(store: &Store, profile: &mut Profile, relay: &Relay) -> Result<()> {
    profile.validate_record()?;
    for _ in 0..32 {
        let response = call(
            relay,
            Method::GET,
            &format!(
                "/v2/node/membership?after={}&limit={PAGE}",
                profile.membership.revision
            ),
            None,
        )
        .await?;
        let changes = response
            .get("changes")
            .and_then(Value::as_array)
            .filter(|v| v.len() <= PAGE)
            .ok_or_else(|| fail("Invalid workspace membership page"))?;
        let mut membership = profile.membership.clone();
        let mut heads = profile.historical_heads.clone();
        for change in changes {
            let change: SignedMembership = parse(change)?;
            cc(membership.apply_next(&change))?;
            heads.insert(membership.revision, membership.head.clone());
        }
        let reported = response["membership_revision"]
            .as_u64()
            .filter(|revision| *revision <= crypto::MAX_WIRE_INTEGER)
            .ok_or_else(|| fail("Invalid workspace membership page"))?;
        let reported_head = response["membership_head"]
            .as_str()
            .ok_or_else(|| fail("Invalid workspace membership page"))?;
        cc(crypto::decode_fixed::<32>(reported_head))?;
        if reported < membership.revision
            || (reported == membership.revision && reported_head != membership.head)
        {
            return Err(fail("Workspace membership rollback or divergence detected"));
        }
        if response["next_after"].is_null() {
            if reported != membership.revision {
                return Err(fail("Workspace membership history is incomplete"));
            }
        } else if changes.is_empty() || response["next_after"].as_u64() != Some(membership.revision)
        {
            return Err(fail("Workspace membership history is incomplete"));
        }
        if membership
            .revoked_device_ids
            .contains(&profile.identity.device.device_id)
            && !response["next_after"].is_null()
        {
            return Err(fail("Workspace retirement history is incomplete"));
        }
        if heads.len() > 8192 {
            return Err(fail("Workspace membership history exceeds its bound"));
        }
        // Do not persist a valid-looking prefix of a malformed or forked page,
        // including a self-tombstone followed by an invalid successor.
        publish_membership(store, profile, membership, heads)?;
        if response["next_after"].is_null() {
            return Ok(());
        }
    }
    Err(fail("Workspace membership update is still in progress"))
}
pub async fn refresh_and_approve(private_dir: &Path, relay: &Relay) -> Result<Profile> {
    let store = io(Store::lock(private_dir, false))?;
    let profile: Profile =
        stored(&store, ACTIVE)?.ok_or_else(|| fail("No encrypted workspace is configured"))?;
    refresh_locked(&store, profile, relay).await
}
async fn refresh_locked(store: &Store, mut profile: Profile, relay: &Relay) -> Result<Profile> {
    profile.validate()?;
    synchronize(store, &mut profile, relay).await?;
    let mut invitations: Vec<RetainedInvitation> = stored(store, INVITATIONS)?.unwrap_or_default();
    if invitations.len() > MAX_INVITATIONS {
        return Err(fail("Invalid workspace invitation state"));
    }
    let now = crate::now() as u64;
    invitations.retain(|i| i.invitation.expires_at > now);
    if !invitations.is_empty() {
        let response = call(relay, Method::GET, "/v2/node/enrollments?limit=100", None).await?;
        let rows = response["enrollments"]
            .as_array()
            .filter(|rows| rows.len() <= PAGE)
            .ok_or_else(|| fail("Invalid workspace enrollment page"))?;
        for row in rows {
            let candidate: EnrollmentCandidate = parse(&row["candidate"])?;
            let Some(index) = invitations
                .iter()
                .position(|i| i.invitation.invitation_id == candidate.invitation_id)
            else {
                continue;
            };
            if candidate
                .verify_invitation(&invitations[index].invitation, now)
                .is_err()
            {
                continue;
            }
            let issuer = cc(Identity::from_record(&profile.identity))?;
            if let Some(approval) = &invitations[index].approval {
                let proposed_head = cc(approval.membership.head())?;
                if let Some(known) = profile
                    .historical_heads
                    .get(&approval.membership.statement.revision)
                {
                    if known == &proposed_head {
                        invitations.remove(index);
                        save(store, INVITATIONS, &invitations)?;
                        continue;
                    }
                    // A verified different entry already occupies the proposal
                    // revision. The stale CAS proposal cannot join our pinned
                    // branch; rebase only after this authenticated evidence.
                    invitations[index].approval = None;
                    save(store, INVITATIONS, &invitations)?;
                }
            }
            if let Some(approval) = &invitations[index].approval {
                cc(approval.verify(&candidate, issuer.public()))?;
            } else {
                let statement = MembershipStatement {
                    workspace_id: profile.membership.workspace_id.clone(),
                    revision: profile.membership.revision + 1,
                    previous_head: profile.membership.head.clone(),
                    issuer_device_id: issuer.public().device_id.clone(),
                    issuer_key_generation: issuer.public().key_generation,
                    action: MembershipAction::Add,
                    device: Some(candidate.device.clone()),
                    revoked_device_id: None,
                };
                let membership = cc(SignedMembership::sign(statement, &issuer))?;
                ensure_advance_fits(&profile, &membership)?;
                let approval = cc(SignedEnrollment::sign(&candidate, membership, &issuer))?;
                invitations[index].approval = Some(approval);
                save(store, INVITATIONS, &invitations)?;
            }
            let approval = invitations[index]
                .approval
                .as_ref()
                .ok_or_else(|| fail("Invalid workspace invitation state"))?;
            let id = &candidate.device.device_id;
            call(
                relay,
                Method::POST,
                &format!("/v2/node/enrollments/{id}/approve"),
                Some(json!({"enrollment":approval})),
            )
            .await?;
            cc(profile.membership.apply_next(&approval.membership))?;
            profile
                .historical_heads
                .insert(profile.membership.revision, profile.membership.head.clone());
            save(store, ACTIVE, &profile)?;
            invitations.remove(index);
            save(store, INVITATIONS, &invitations)?;
        }
    }
    save(store, INVITATIONS, &invitations)?;
    Ok(profile)
}

pub async fn manage(
    action: &str,
    config_path: &Path,
    input: Value,
    allow_insecure_test: bool,
) -> Result<Value> {
    if action == "status" {
        return public_status(config_path);
    }
    let (config, revision) = workspace_cli::read_config(config_path)?;
    if input["expected_revision"].as_str() != Some(&revision) {
        return Err(fail(
            "Workspace configuration changed; reload before continuing",
        ));
    }
    let private_dir = private_directory(config_path)?;
    if config.get("encrypted_workspace").is_some() {
        validate_marker(&config, &private_dir)?;
        let profile = load_record(&private_dir)?;
        if validate_config_profile_binding(&config, &private_dir, &profile).is_err()
            && !(action == "check" && pending_activation_matches(&config, &revision, &private_dir)?)
        {
            return Err(fail(
                "Encrypted workspace configuration does not match its private identity",
            ));
        }
    } else if [
        "invite-computer",
        "invite-phone",
        "cancel-invite",
        "list-devices",
        "revoke-device",
        "start",
        "stop",
    ]
    .contains(&action)
    {
        return Err(fail("Create or join an encrypted workspace first"));
    }
    if action == "start" {
        let failed_start = input
            .get("failed_start_revision")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| fail("Invalid failed Start revision"))
            })
            .transpose()?;
        let profile = load_record(&private_dir)?;
        profile.validate()?;
        if let Some(status) = crate::workspace_service::start(
            config_path,
            &private_dir,
            &profile.identity.device.device_id,
            &revision,
            failed_start,
        )
        .await?
        {
            return Ok(status);
        }
        return crate::workspace_lifecycle::manage_start(config_path, &input).await;
    }
    if action == "stop" {
        return crate::workspace_lifecycle::manage_stop(config_path, &input).await;
    }
    let store = io(Store::lock(&private_dir, true))?;
    // Another manager may have changed the config while this process was
    // waiting for the private-state lock. Never act on that earlier binding.
    if workspace_cli::read_config(config_path)?.1 != revision {
        return Err(fail(
            "Workspace configuration changed; reload before continuing",
        ));
    }
    if config.get("encrypted_workspace").is_some() {
        let current: Profile =
            stored(&store, ACTIVE)?.ok_or_else(|| fail("No encrypted workspace is configured"))?;
        if validate_config_profile_binding(&config, &private_dir, &current).is_err()
            && !(action == "check" && pending_activation_matches(&config, &revision, &private_dir)?)
        {
            return Err(fail(
                "Encrypted workspace configuration does not match its private identity",
            ));
        }
    }
    // Invitation polling also uses Check. Only completing this invocation's
    // exact saved enrollment may start a connector; active-profile refresh
    // must never undo an intentional Stop.
    let start_after_activation =
        matches!(action, "create" | "join" | "switch-create" | "switch-join")
            || (action == "check"
                && stored::<Option<Pending>>(&store, PENDING)?
                    .flatten()
                    .is_some());
    let result = match action {
        "create" | "join" | "switch-create" | "switch-join" => {
            if !action.starts_with("switch-")
                && (config.get("node_token").is_some()
                    || config.get("encrypted_workspace").is_some())
            {
                return Err(fail(
                    "Use Change workspace for an already configured computer",
                ));
            }
            if stored::<Option<Pending>>(&store, PENDING)?
                .flatten()
                .is_some()
            {
                return Err(fail(
                    "A workspace enrollment is already pending; check its status",
                ));
            }
            let server = crate::url::checked_url(
                &text(&input, "server_url", crate::url::CURRENT)?,
                allow_insecure_test,
            )?;
            let identity = cc(Identity::generate(
                uuid::Uuid::new_v4().to_string(),
                Role::Computer,
            ))?;
            let previous_profile = if config.get("encrypted_workspace").is_some() {
                stored(&store, ACTIVE)?
            } else {
                None
            };
            let mut pending = Pending {
                version: 1,
                action: action.into(),
                expected_revision: revision,
                server_url: server,
                workspace_name: text(&input, "workspace_name", "Workspace")?,
                computer_name: text(&input, "computer_name", "Computer")?,
                node_token: service_token()?,
                identity: identity.to_record(),
                genesis: None,
                candidate: None,
                invitation: None,
                profile: None,
                previous_profile,
                retirement: None,
                legacy_retirement_request: None,
                legacy_retirement_status: None,
                legacy_service_handover: false,
            };
            pending.legacy_service_handover = action.starts_with("switch-")
                && config.get("node_token").is_some()
                && input
                    .get("allow_legacy_service_handover")
                    .map_or(Ok(false), |value| {
                        value
                            .as_bool()
                            .ok_or_else(|| fail("Invalid workspace input"))
                    })?;
            if action.ends_with("create") {
                pending.genesis = Some(cc(SignedMembership::sign(
                    MembershipStatement {
                        workspace_id: uuid::Uuid::new_v4().to_string(),
                        revision: 0,
                        previous_head: crypto::encode(&[0; 32]),
                        issuer_device_id: identity.public().device_id.clone(),
                        issuer_key_generation: 1,
                        action: MembershipAction::Add,
                        device: Some(identity.public().clone()),
                        revoked_device_id: None,
                    },
                    &identity,
                ))?);
            } else {
                let code = input["code"]
                    .as_str()
                    .ok_or_else(|| fail("Enter a computer invitation from an enrolled computer"))?;
                if code.len() > crypto::MAX_INVITATION_CODE_BYTES {
                    return Err(fail("Invalid computer invitation"));
                }
                let bytes = cc(crypto::decode(
                    code.strip_prefix("ze1.")
                        .ok_or_else(|| fail("Invalid computer invitation"))?,
                    3072,
                ))?;
                let invitation: Invitation = decode_record(&bytes)?;
                cc(invitation.validate())?;
                if invitation.purpose != Role::Computer {
                    return Err(fail("Use a computer invitation from an enrolled computer"));
                }
                pending.candidate = Some(cc(EnrollmentCandidate::create(
                    &invitation,
                    &identity,
                    crate::now() as u64,
                ))?);
                pending.invitation = Some(invitation);
            }
            save(&store, PENDING, &Some(&pending))?;
            resume(
                &store,
                config_path,
                &config,
                pending,
                allow_insecure_test,
                false,
            )
            .await
        }
        "check" => {
            if let Some(mut pending) = stored::<Option<Pending>>(&store, PENDING)?.flatten() {
                if input
                    .get("allow_legacy_service_handover")
                    .map_or(Ok(false), |value| {
                        value
                            .as_bool()
                            .ok_or_else(|| fail("Invalid workspace input"))
                    })?
                {
                    if config.get("node_token").is_some() && pending.previous_profile.is_none() {
                        pending.legacy_service_handover = true;
                    }
                    save(&store, PENDING, &Some(&pending))?;
                }
                let allow_local_disconnect = input
                    .get("allow_unconfirmed_legacy_retirement")
                    .map_or(Ok(false), |value| {
                        value
                            .as_bool()
                            .ok_or_else(|| fail("Invalid workspace input"))
                    })?;
                resume(
                    &store,
                    config_path,
                    &config,
                    pending,
                    allow_insecure_test,
                    allow_local_disconnect,
                )
                .await
            } else {
                validate_marker(&config, &private_dir)?;
                let profile: Profile = stored(&store, ACTIVE)?
                    .ok_or_else(|| fail("No encrypted workspace is configured"))?;
                validate_config_profile_binding(&config, &private_dir, &profile)?;
                let relay = relay(&profile, allow_insecure_test)?;
                let mut report = refresh_locked(&store, profile, &relay).await?.report();
                report["config_revision"] = json!(revision);
                Ok(report)
            }
        }
        "invite-computer" | "invite-phone" => {
            let mut profile: Profile = stored(&store, ACTIVE)?
                .ok_or_else(|| fail("Create or join an encrypted workspace first"))?;
            profile.validate()?;
            let relay = relay(&profile, allow_insecure_test)?;
            synchronize(&store, &mut profile, &relay).await?;
            let issuer = cc(Identity::from_record(&profile.identity))?;
            let purpose = if action == "invite-computer" {
                Role::Computer
            } else {
                Role::Phone
            };
            let invitation = cc(Invitation::generate(
                profile.membership.workspace_id.clone(),
                uuid::Uuid::new_v4().to_string(),
                purpose,
                issuer.public().clone(),
                crate::now() as u64 + 600,
                profile.membership.revision,
                profile.membership.head.clone(),
            ))?;
            let bootstrap = cc(SignedMembershipSnapshot::sign(
                profile.membership.clone(),
                &issuer,
            ))?;
            let mut invitations: Vec<RetainedInvitation> =
                stored(&store, INVITATIONS)?.unwrap_or_default();
            invitations.retain(|i| i.invitation.expires_at > crate::now() as u64);
            if invitations.len() >= MAX_INVITATIONS {
                return Err(fail("Too many pending workspace invitations"));
            }
            invitations.push(RetainedInvitation {
                invitation: invitation.clone(),
                bootstrap: bootstrap.clone(),
                approval: None,
            });
            save(&store, INVITATIONS, &invitations)?;
            call(&relay,Method::POST,"/v2/node/invitations",Some(json!({"invitation_id":invitation.invitation_id,"purpose":purpose,"expires_at":invitation.expires_at,"bootstrap":bootstrap}))).await?;
            let code = format!(
                "ze1.{}",
                crypto::encode(
                    &serde_json::to_vec(&invitation)
                        .map_err(|_| fail("Invitation could not be encoded"))?
                )
            );
            Ok(
                json!({"code":code,"server_url":profile.server_url,"invitation_id":invitation.invitation_id,"expires_at":invitation.expires_at,"purpose":purpose}),
            )
        }
        "cancel-invite" => {
            let id = text(&input, "invitation_id", "")?;
            let mut invitations: Vec<RetainedInvitation> =
                stored(&store, INVITATIONS)?.unwrap_or_default();
            invitations.retain(|i| i.invitation.invitation_id != id);
            save(&store, INVITATIONS, &invitations)?;
            Ok(json!({"cancelled":true}))
        }
        "list-devices" | "revoke-device" => {
            let mut profile: Profile = stored(&store, ACTIVE)?
                .ok_or_else(|| fail("Create or join an encrypted workspace first"))?;
            profile.validate()?;
            let relay = relay(&profile, allow_insecure_test)?;
            synchronize(&store, &mut profile, &relay).await?;
            if action == "revoke-device" {
                let target = text(&input, "device_id", "")?;
                if target == profile.identity.device.device_id {
                    return Err(fail("Use Change workspace to retire this computer"));
                }
                let mut existing: Option<SignedMembership> =
                    stored::<Option<SignedMembership>>(&store, "revocation.json")?.flatten();
                if let Some(change) = &existing {
                    if change.statement.revoked_device_id.as_deref() != Some(&target) {
                        return Err(fail("Another device removal is still pending"));
                    }
                    if let Some(known) = profile.historical_heads.get(&change.statement.revision) {
                        if known != &cc(change.head())? {
                            // Only a verified competing entry proves the old
                            // CAS proposal cannot have committed on our branch.
                            save::<Option<SignedMembership>>(&store, "revocation.json", &None)?;
                            existing = None;
                        }
                    }
                }
                if profile.membership.revoked_device_ids.contains(&target) {
                    save::<Option<SignedMembership>>(&store, "revocation.json", &None)?;
                } else {
                    let change = if let Some(change) = existing {
                        change
                    } else {
                        let identity = cc(Identity::from_record(&profile.identity))?;
                        let change = cc(SignedMembership::sign(
                            MembershipStatement {
                                workspace_id: profile.membership.workspace_id.clone(),
                                revision: profile.membership.revision + 1,
                                previous_head: profile.membership.head.clone(),
                                issuer_device_id: identity.public().device_id.clone(),
                                issuer_key_generation: identity.public().key_generation,
                                action: MembershipAction::Revoke,
                                device: None,
                                revoked_device_id: Some(target),
                            },
                            &identity,
                        ))?;
                        ensure_advance_fits(&profile, &change)?;
                        save(&store, "revocation.json", &Some(&change))?;
                        change
                    };
                    let head = cc(change.head())?;
                    if profile.historical_heads.get(&change.statement.revision) != Some(&head) {
                        call(
                            &relay,
                            Method::POST,
                            "/v2/node/membership",
                            Some(json!({"membership":change})),
                        )
                        .await?;
                        cc(profile.membership.apply_next(&change))?;
                        profile
                            .historical_heads
                            .insert(profile.membership.revision, profile.membership.head.clone());
                        save(&store, ACTIVE, &profile)?;
                    }
                    save::<Option<SignedMembership>>(&store, "revocation.json", &None)?;
                }
            }
            Ok(
                json!({"devices":profile.membership.members.values().map(|m|json!({"device_id":m.device_id,"role":m.role,"is_self":m.device_id==profile.identity.device.device_id})).collect::<Vec<_>>(),"membership_revision":profile.membership.revision}),
            )
        }
        _ => Err(fail("Unknown workspace action")),
    };
    drop(store);
    let mut result = result?;
    if start_after_activation && result["state"] == "active" {
        if let (Some(node), Some(revision)) = (
            result["node_id"].as_str(),
            result["config_revision"].as_str(),
        ) {
            if let Some(lifecycle) =
                crate::workspace_service::start(config_path, &private_dir, node, revision, None)
                    .await?
            {
                result["connector_state"] = lifecycle["state"].clone();
                result["running"] = lifecycle["running"].clone();
                result["generation"] = lifecycle["generation"].clone();
            }
        }
    }
    Ok(result)
}

async fn resume(
    store: &Store,
    config_path: &Path,
    config: &Value,
    mut pending: Pending,
    allow_insecure_test: bool,
    allow_local_legacy_disconnect: bool,
) -> Result<Value> {
    let current_revision = workspace_cli::read_config(config_path)?.1;
    if let Some(profile) = &pending.profile {
        if config["encrypted_workspace"]["version"] == 1
            && config["encrypted_workspace"]["state_dir"] == json!(private_directory(config_path)?)
            && config["server_url"] == profile.server_url
            && config["state_dir"] == json!(profile.state_dir)
            && config.get("node_token").is_none()
        {
            let active: Profile = stored(store, ACTIVE)?
                .ok_or_else(|| fail("Pending workspace activation is incomplete"))?;
            if active.identity.device != profile.identity.device {
                return Err(fail(
                    "Pending workspace activation conflicts with active state",
                ));
            }
            active.validate()?;
            crate::workspace_service::activated(
                store,
                &active.identity.device.device_id,
                &current_revision,
            )?;
            save::<Option<Pending>>(store, PENDING, &None)?;
            let mut result = active.report();
            result["config_revision"] = json!(current_revision);
            if pending.legacy_retirement_status.as_deref() == Some("locally-disconnected") {
                result["legacy_retirement"] = json!("unconfirmed");
            }
            return Ok(result);
        }
    }
    if pending.version != 1 || current_revision != pending.expected_revision {
        return Err(fail(
            "Workspace configuration changed; pending enrollment was preserved",
        ));
    }
    let relay = relay_for(
        &pending.server_url,
        &pending.node_token,
        allow_insecure_test,
    )?;
    if pending.profile.is_none() {
        let membership = if let Some(genesis) = &pending.genesis {
            call(
                &relay,
                Method::POST,
                "/v2/workspaces",
                Some(json!({"membership":genesis,"service_token":pending.node_token})),
            )
            .await?;
            cc(MembershipState::from_genesis(genesis))?
        } else {
            let candidate = pending
                .candidate
                .as_ref()
                .ok_or_else(|| fail("Invalid pending enrollment"))?;
            call(
                &relay,
                Method::POST,
                "/v2/enrollments",
                Some(json!({"candidate":candidate,"service_token":pending.node_token})),
            )
            .await?;
            let status = call(
                &relay,
                Method::GET,
                &format!("/v2/enrollments/{}", pending.identity.device.device_id),
                None,
            )
            .await?;
            if status["state"] != "active" {
                return Ok(
                    json!({"state":"pending","node_id":pending.identity.device.device_id,"config_revision":pending.expected_revision}),
                );
            }
            let invitation = pending
                .invitation
                .as_ref()
                .ok_or_else(|| fail("Invalid pending enrollment"))?;
            let snapshot: SignedMembershipSnapshot = parse(&status["bootstrap"])?;
            let mut state = cc(snapshot.verify_bootstrap(invitation))?;
            let approval: SignedEnrollment = parse(&status["enrollment"])?;
            cc(approval.verify(candidate, &invitation.issuer))?;
            // The newly activated token can now read the public history. The
            // code-pinned bootstrap is the only trust source for the first page.
            for _ in 0..32 {
                if state.revision >= approval.membership.statement.revision {
                    break;
                }
                let page = call(
                    &relay,
                    Method::GET,
                    &format!("/v2/node/membership?after={}&limit={PAGE}", state.revision),
                    None,
                )
                .await?;
                let changes = page["changes"]
                    .as_array()
                    .filter(|v| !v.is_empty() && v.len() <= PAGE)
                    .ok_or_else(|| fail("Workspace enrollment history is incomplete"))?;
                for change in changes {
                    let change: SignedMembership = parse(change)?;
                    if change.statement.revision > approval.membership.statement.revision {
                        break;
                    }
                    cc(state.apply_next(&change))?;
                }
            }
            if state.revision != approval.membership.statement.revision
                || state.head != cc(approval.membership.head())?
                || state.members.get(&pending.identity.device.device_id)
                    != Some(&pending.identity.device)
            {
                return Err(fail(
                    "Workspace enrollment is not in the verified membership history",
                ));
            }
            state
        };
        let state_dir = private_directory(config_path)?
            .join(format!("receipts-{}", pending.identity.device.device_id));
        let profile = Profile {
            server_url: pending.server_url.clone(),
            workspace_name: pending.workspace_name.clone(),
            computer_name: pending.computer_name.clone(),
            node_token: pending.node_token.clone(),
            identity: cc(Identity::from_record(&pending.identity))?.to_record(),
            historical_heads: BTreeMap::from([(membership.revision, membership.head.clone())]),
            membership,
            state_dir,
        };
        profile.validate()?;
        pending.profile = Some(profile);
        save(store, PENDING, &Some(&pending))?;
    }
    // Switch retirement and config activation are completed below. Keep the
    // exact newly admitted profile private until the previous endpoint is safe.
    activate(
        store,
        config_path,
        config,
        pending,
        allow_insecure_test,
        allow_local_legacy_disconnect,
    )
    .await
}
async fn activate(
    store: &Store,
    config_path: &Path,
    config: &Value,
    mut pending: Pending,
    allow_insecure_test: bool,
    allow_local_legacy_disconnect: bool,
) -> Result<Value> {
    let private_dir = private_directory(config_path)?;
    let old = pending.previous_profile.take();
    let legacy = if old.is_none() && config.get("node_token").is_some() {
        if !pending.action.starts_with("switch-") {
            return Err(fail(
                "Use Change workspace for an already configured computer",
            ));
        }
        let encoded = serde_json::to_string(config)
            .map_err(|_| fail("Invalid legacy connector configuration"))?;
        let parsed = crate::config::parse(&encoded)?;
        Some(crate::config::validate(
            &parsed,
            allow_insecure_test,
            &io(crate::config::Environment::current())?,
        )?)
    } else {
        None
    };
    // The old journal stays untouched. Holding its existing lock through
    // retirement and config CAS prevents a stopped process from being replaced
    // by another connector while this transition is committing.
    let old_state_dir = old
        .as_ref()
        .map(|p| p.state_dir.as_path())
        .or_else(|| legacy.as_ref().map(|p| p.state_dir.as_path()));
    if let Some(legacy) = legacy.as_ref().filter(|_| pending.legacy_service_handover) {
        let binding = crate::workspace_service::Binding {
            revision: pending.expected_revision.clone(),
            node_id: pending.identity.device.device_id.clone(),
            config: config_path.to_path_buf(),
            state: legacy.state_dir.clone(),
            relay_binding: legacy.binding.clone(),
        };
        if let Some(migration) = crate::workspace_service::drain(store, binding).await? {
            return Ok(
                json!({"state":if migration["phase"]=="draining"{"migration-draining"}else{"migration-blocked"},"encrypted":false,"config_revision":pending.expected_revision,"node_id":pending.identity.device.device_id,"migration":migration}),
            );
        }
    }
    if let Some(previous) = &old {
        crate::workspace_service::prepare_switch(
            store,
            config_path,
            &pending.expected_revision,
            &previous.identity.device.device_id,
            &pending.identity.device.device_id,
        )
        .await?;
    }
    let _journal = if let Some(state_dir) = old_state_dir {
        let stop = || {
            if legacy.is_some() {
                fail("Stop the legacy connector using its service controls, then Check connection")
            } else {
                fail("Stop the connector before changing workspace")
            }
        };
        let lock_path = state_dir.join(crate::journal::LOCK);
        if !matches!(
            crate::fsguard::probe_lock(&lock_path),
            crate::fsguard::LockState::Free | crate::fsguard::LockState::Absent
        ) {
            return Err(stop());
        }
        let lock = crate::fsguard::private_file(&lock_path, false).map_err(|_| stop())?;
        if let Some(lock) = lock {
            crate::fsguard::lock_exclusive(&lock).map_err(|_| stop())?;
            Some(lock)
        } else {
            None
        }
    } else {
        None
    };
    if let Some(mut old) = old {
        if !pending.action.starts_with("switch-") {
            return Err(fail(
                "Use Change workspace for an already configured computer",
            ));
        }
        old.validate_record()?;
        if let Some(current) = stored::<Profile>(store, ACTIVE)? {
            current.validate_record()?;
            if current.identity.device == old.identity.device {
                if current.server_url != old.server_url
                    || current.node_token != old.node_token
                    || current.state_dir != old.state_dir
                    || current.membership.workspace_id != old.membership.workspace_id
                    || current.membership.revision < old.membership.revision
                    || current.historical_heads.get(&old.membership.revision)
                        != Some(&old.membership.head)
                {
                    return Err(fail(
                        "Pending retirement conflicts with verified workspace history",
                    ));
                }
                // A still-running old connector may have pinned more history
                // while destination enrollment was waiting for approval.
                old = current;
            } else if pending
                .profile
                .as_ref()
                .is_none_or(|destination| destination.identity.device != current.identity.device)
            {
                return Err(fail(
                    "Pending retirement conflicts with active workspace state",
                ));
            }
        }
        if !old.retired() {
            let old_relay = relay(&old, allow_insecure_test)?;
            // Even an existing proposal must reconcile against authenticated
            // history. A CAS409 or401 alone never proves it stale or retired.
            let synchronized = synchronize_record(store, &mut old, &old_relay).await;
            pending.previous_profile = Some(old);
            save(store, PENDING, &Some(&pending))?;
            synchronized?;
            old = pending
                .previous_profile
                .take()
                .ok_or_else(|| fail("Invalid workspace retirement state"))?;
        }
        if let Some(change) = &pending.retirement {
            cc(change.verify_signature(&old.identity.device))?;
            if change.statement.workspace_id != old.membership.workspace_id
                || change.statement.action != MembershipAction::Revoke
                || change.statement.revoked_device_id.as_deref()
                    != Some(old.identity.device.device_id.as_str())
            {
                return Err(fail("Invalid workspace retirement state"));
            }
            let proposed_head = cc(change.head())?;
            if let Some(known) = old.historical_heads.get(&change.statement.revision) {
                if known != &proposed_head {
                    // Only a verified competing entry at this exact revision
                    // permits replacing a previously persisted proposal.
                    pending.retirement = None;
                } else if !old.retired() {
                    return Err(fail(
                        "Verified retirement is inconsistent with workspace state",
                    ));
                }
            } else if !old.retired() {
                cc(change.verify_successor(
                    &old.membership.workspace_id,
                    old.membership.revision,
                    &old.membership.head,
                    &old.identity.device,
                ))?;
            } else {
                return Err(fail(
                    "Pending retirement is absent from verified workspace history",
                ));
            }
        }
        if !old.retired() {
            let old_relay = relay(&old, allow_insecure_test)?;
            if pending.retirement.is_none() {
                let identity = cc(Identity::from_record(&old.identity))?;
                let retirement = cc(SignedMembership::sign(
                    MembershipStatement {
                        workspace_id: old.membership.workspace_id.clone(),
                        revision: old.membership.revision + 1,
                        previous_head: old.membership.head.clone(),
                        issuer_device_id: identity.public().device_id.clone(),
                        issuer_key_generation: identity.public().key_generation,
                        action: MembershipAction::Revoke,
                        device: None,
                        revoked_device_id: Some(identity.public().device_id.clone()),
                    },
                    &identity,
                ))?;
                ensure_advance_fits(&old, &retirement)?;
                pending.retirement = Some(retirement);
            }
            pending.previous_profile = Some(old);
            save(store, PENDING, &Some(&pending))?;
            old = pending
                .previous_profile
                .take()
                .ok_or_else(|| fail("Invalid workspace retirement state"))?;
            call(
                &old_relay,
                Method::DELETE,
                "/v2/node/self",
                Some(json!({"membership":pending.retirement})),
            )
            .await?;
            // The existing retired-credential history endpoint is restricted
            // to this node's own signed tombstone. Verify that proof even when
            // the DELETE response arrived; never activate on an unsigned hint.
            let synchronized = synchronize_record(store, &mut old, &old_relay).await;
            pending.previous_profile = Some(old);
            save(store, PENDING, &Some(&pending))?;
            synchronized?;
            old = pending
                .previous_profile
                .take()
                .ok_or_else(|| fail("Invalid workspace retirement state"))?;
            let retirement = pending
                .retirement
                .as_ref()
                .ok_or_else(|| fail("Invalid workspace retirement state"))?;
            if !old.retired()
                || old.historical_heads.get(&retirement.statement.revision)
                    != Some(&cc(retirement.head())?)
            {
                return Err(fail("Workspace retirement is absent from verified history"));
            }
        }
        save(
            store,
            &format!("retired-{}.json", old.identity.device.device_id),
            &old,
        )?;
    } else if let Some(legacy) = legacy {
        if pending.legacy_retirement_request.is_none() {
            pending.legacy_retirement_request = Some(uuid::Uuid::new_v4().to_string());
            save(store, PENDING, &Some(&pending))?;
        }
        let request_id = pending
            .legacy_retirement_request
            .clone()
            .ok_or_else(|| fail("Invalid legacy retirement state"))?;
        if allow_local_legacy_disconnect
            && pending.legacy_retirement_status.as_deref() == Some("unconfirmed")
        {
            pending.legacy_retirement_status = Some("locally-disconnected".into());
            save(store, PENDING, &Some(&pending))?;
        }
        if pending.legacy_retirement_status.as_deref() != Some("locally-disconnected") {
            let old_relay = relay_for(&legacy.server_url, &legacy.token, allow_insecure_test)?;
            let receipt = call(
                &old_relay,
                Method::DELETE,
                "/v1/node/self",
                Some(json!({"request_id":request_id})),
            )
            .await;
            let confirmed = receipt.is_ok_and(|receipt| {
                receipt["retired"] == true
                    && receipt["request_id"].as_str() == Some(&request_id)
                    && receipt["node_id"]
                        .as_str()
                        .is_some_and(|node| !node.is_empty())
            });
            if !confirmed {
                pending.legacy_retirement_status = Some("unconfirmed".into());
                save(store, PENDING, &Some(&pending))?;
                return Ok(
                    json!({"state":"legacy-retirement-unconfirmed","config_revision":pending.expected_revision,"node_id":pending.identity.device.device_id}),
                );
            }
            pending.legacy_retirement_status = Some("confirmed".into());
            save(store, PENDING, &Some(&pending))?;
        }
        // The private archive retains the previous binding for inspection;
        // neither it nor its journal is ever selected by the new connector.
        save(store, &format!("legacy-retired-{request_id}.json"), config)?;
    }
    let profile = pending
        .profile
        .as_ref()
        .ok_or_else(|| fail("Invalid pending workspace state"))?;
    let mut updated = config.clone();
    let object = updated
        .as_object_mut()
        .ok_or_else(|| fail("Invalid connector configuration"))?;
    object.remove("node_token");
    object.remove("identity_url");
    object.insert("server_url".into(), json!(profile.server_url));
    object.insert("state_dir".into(), json!(profile.state_dir));
    object.entry("hgs_path").or_insert(json!("hgs"));
    object.insert(
        "encrypted_workspace".into(),
        json!({"version":1,"state_dir":private_dir}),
    );
    // Publish profile before config: a crash is resumable from pending, while
    // the legacy connector still cannot observe a usable protected bearer.
    save(store, ACTIVE, profile)?;
    let revision =
        workspace_cli::replace_config(config_path, &pending.expected_revision, &updated)?;
    crate::workspace_service::activated(store, &profile.identity.device.device_id, &revision)?;
    save::<Option<Pending>>(store, PENDING, &None)?;
    let mut result = profile.report();
    result["config_revision"] = json!(revision);
    if pending.legacy_retirement_status.as_deref() == Some("locally-disconnected") {
        result["legacy_retirement"] = json!("unconfirmed");
    }
    Ok(result)
}

/// Private synthetic fixture setup; absent from packaged builds. The actual
/// Check/resume path consumes this exact persisted Pending without HTTP enroll.
#[cfg(all(target_os = "linux", feature = "test-hooks"))]
pub fn test_reserve_legacy_destination(config: &Path, profile: Profile) -> Result<Value> {
    let (_, revision) = workspace_cli::read_config(config)?;
    let identity = serde_json::from_value(
        serde_json::to_value(&profile.identity)
            .map_err(|_| fail("Invalid synthetic pending fixture"))?,
    )
    .map_err(|_| fail("Invalid synthetic pending fixture"))?;
    let pending = Pending {
        version: 1,
        action: "switch-create".into(),
        expected_revision: revision,
        server_url: profile.server_url.clone(),
        workspace_name: profile.workspace_name.clone(),
        computer_name: profile.computer_name.clone(),
        node_token: profile.node_token.clone(),
        identity,
        genesis: None,
        candidate: None,
        invitation: None,
        profile: Some(profile),
        previous_profile: None,
        retirement: None,
        legacy_retirement_request: None,
        legacy_retirement_status: None,
        legacy_service_handover: true,
    };
    let store = io(Store::lock(&private_directory(config)?, true))?;
    save(&store, PENDING, &Some(&pending))?;
    serde_json::to_value(pending).map_err(|_| fail("Invalid synthetic pending fixture"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready(config: &Path, revision: String) -> Pending {
        let identity =
            Identity::generate(uuid::Uuid::new_v4().to_string(), Role::Computer).unwrap();
        let genesis = SignedMembership::sign(
            MembershipStatement {
                workspace_id: uuid::Uuid::new_v4().to_string(),
                revision: 0,
                previous_head: crypto::encode(&[0; 32]),
                issuer_device_id: identity.public().device_id.clone(),
                issuer_key_generation: 1,
                action: MembershipAction::Add,
                device: Some(identity.public().clone()),
                revoked_device_id: None,
            },
            &identity,
        )
        .unwrap();
        let membership = MembershipState::from_genesis(&genesis).unwrap();
        let profile = Profile {
            server_url: "https://relay.example.test".into(),
            workspace_name: "Synthetic".into(),
            computer_name: "Synthetic".into(),
            node_token: crypto::encode(&[41; 32]),
            identity: identity.to_record(),
            historical_heads: BTreeMap::from([(0, membership.head.clone())]),
            membership,
            state_dir: private_directory(config)
                .unwrap()
                .join(format!("receipts-{}", identity.public().device_id)),
        };
        Pending {
            version: 1,
            action: "create".into(),
            expected_revision: revision,
            server_url: profile.server_url.clone(),
            workspace_name: profile.workspace_name.clone(),
            computer_name: profile.computer_name.clone(),
            node_token: profile.node_token.clone(),
            identity: identity.to_record(),
            genesis: Some(genesis),
            candidate: None,
            invitation: None,
            profile: Some(profile),
            previous_profile: None,
            retirement: None,
            legacy_retirement_request: None,
            legacy_retirement_status: None,
            legacy_service_handover: false,
        }
    }
    #[cfg(unix)]
    #[test]
    fn failed_membership_publication_does_not_advance_pending_retirement_pin() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("connector.json");
        let private = private_directory(&config).unwrap();
        let store = Store::lock(&private, true).unwrap();
        let mut pending = ready(&config, String::new());
        pending.action = "switch-create".into();
        let mut old = ready(&config, String::new()).profile.take().unwrap();
        let previous_membership = old.membership.clone();
        let previous_heads = old.historical_heads.clone();
        let identity = Identity::from_record(&old.identity).unwrap();
        let retirement = SignedMembership::sign(
            MembershipStatement {
                workspace_id: old.membership.workspace_id.clone(),
                revision: old.membership.revision + 1,
                previous_head: old.membership.head.clone(),
                issuer_device_id: identity.public().device_id.clone(),
                issuer_key_generation: identity.public().key_generation,
                action: MembershipAction::Revoke,
                device: None,
                revoked_device_id: Some(identity.public().device_id.clone()),
            },
            &identity,
        )
        .unwrap();
        pending.retirement = Some(retirement.clone());
        save(&store, ACTIVE, &old).unwrap();
        pending.previous_profile = Some(old);
        save(&store, PENDING, &Some(&pending)).unwrap();
        let destination = serde_json::to_value(&pending.profile).unwrap();
        old = pending.previous_profile.take().unwrap();
        let mut membership = old.membership.clone();
        membership.apply_next(&retirement).unwrap();
        let mut heads = old.historical_heads.clone();
        heads.insert(membership.revision, membership.head.clone());
        // Store refuses a multiply-linked ACTIVE, while PENDING remains
        // writable. This is the failed-publication/successful-pending-write
        // ordering that used to leave the durable pending pin ahead forever.
        let alias = private.join("active-alias.json");
        std::fs::hard_link(private.join(ACTIVE), &alias).unwrap();
        assert!(publish_membership(&store, &mut old, membership.clone(), heads.clone()).is_err());
        assert_eq!(old.membership, previous_membership);
        assert_eq!(old.historical_heads, previous_heads);
        pending.previous_profile = Some(old);
        save(&store, PENDING, &Some(&pending)).unwrap();
        let mut reloaded = stored::<Option<Pending>>(&store, PENDING)
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(&reloaded.profile).unwrap(),
            destination
        );
        assert_eq!(reloaded.retirement.as_ref(), Some(&retirement));
        old = reloaded.previous_profile.take().unwrap();
        assert_eq!(old.membership, previous_membership);
        assert_eq!(old.historical_heads, previous_heads);
        std::fs::remove_file(alias).unwrap();
        assert_eq!(
            stored::<Profile>(&store, ACTIVE)
                .unwrap()
                .unwrap()
                .membership,
            old.membership
        );
        publish_membership(&store, &mut old, membership, heads).unwrap();
        assert!(old.retired());
        assert_eq!(
            stored::<Profile>(&store, ACTIVE)
                .unwrap()
                .unwrap()
                .membership,
            old.membership
        );
        assert_eq!(
            serde_json::to_value(&reloaded.profile).unwrap(),
            destination
        );
    }
    #[tokio::test]
    async fn committed_profile_before_config_resumes_without_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("connector.json");
        let revision = workspace_cli::replace_config(
            &config,
            "",
            &json!({"extension":{"keep":true},"hgs_path":"/opt/example/hgs"}),
        )
        .unwrap();
        let private = private_directory(&config).unwrap();
        let store = Store::lock(&private, true).unwrap();
        let pending = ready(&config, revision.clone());
        let id = pending.identity.device.device_id.clone();
        save(&store, PENDING, &Some(&pending)).unwrap();
        save(&store, ACTIVE, pending.profile.as_ref().unwrap()).unwrap();
        drop(store);
        let result = manage(
            "check",
            &config,
            json!({"expected_revision":revision}),
            false,
        )
        .await
        .unwrap();
        assert_eq!(result["node_id"], id);
        let (config, _) = workspace_cli::read_config(&config).unwrap();
        assert_eq!(config["extension"]["keep"], true);
        assert_eq!(config["hgs_path"], "/opt/example/hgs");
        assert!(config.get("node_token").is_none());
        let store = Store::lock(&private, false).unwrap();
        assert!(stored::<Option<Pending>>(&store, PENDING)
            .unwrap()
            .flatten()
            .is_none());
    }
    #[tokio::test]
    async fn committed_config_before_pending_cleanup_is_recognized() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("connector.json");
        let private = private_directory(&config).unwrap();
        let store = Store::lock(&private, true).unwrap();
        let pending = ready(&config, String::new());
        let profile = pending.profile.as_ref().unwrap();
        let id = profile.identity.device.device_id.clone();
        save(&store, PENDING, &Some(&pending)).unwrap();
        save(&store, ACTIVE, profile).unwrap();
        let revision=workspace_cli::replace_config(&config,"",&json!({"server_url":profile.server_url,"state_dir":profile.state_dir,"encrypted_workspace":{"version":1,"state_dir":private},"hgs_path":"hgs"})).unwrap();
        drop(store);
        let result = manage(
            "check",
            &config,
            json!({"expected_revision":revision}),
            false,
        )
        .await
        .unwrap();
        assert_eq!(result["node_id"], id);
        let store = Store::lock(&private, false).unwrap();
        assert!(stored::<Option<Pending>>(&store, PENDING)
            .unwrap()
            .flatten()
            .is_none());
    }
    #[tokio::test]
    async fn malformed_protected_marker_cannot_use_or_change_default_profile() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("connector.json");
        let private = private_directory(&config).unwrap();
        let store = Store::lock(&private, true).unwrap();
        let pending = ready(&config, String::new());
        let profile = pending.profile.as_ref().unwrap();
        save(&store, ACTIVE, profile).unwrap();
        let before = store.read(ACTIVE).unwrap().unwrap();
        drop(store);
        let base = json!({"server_url":profile.server_url,"state_dir":profile.state_dir,"encrypted_workspace":{"version":1,"state_dir":private},"hgs_path":"hgs"});
        let mut variants = Vec::new();
        let mut v = base.clone();
        v["encrypted_workspace"] = Value::Null;
        variants.push(v);
        let mut v = base.clone();
        v["encrypted_workspace"]["version"] = json!(2);
        variants.push(v);
        let mut v = base.clone();
        v["encrypted_workspace"]["state_dir"] = json!(dir.path().join("other-private"));
        variants.push(v);
        let mut v = base.clone();
        v["encrypted_workspace"]["extra"] = json!(true);
        variants.push(v);
        let mut v = base.clone();
        v["node_token"] = json!("legacy-token-must-not-coexist");
        variants.push(v);
        let mut v = base.clone();
        v["server_url"] = json!("https://other.example.test");
        variants.push(v);
        let mut v = base;
        v["state_dir"] = json!(dir.path().join("other-receipts"));
        variants.push(v);
        let mut revision = String::new();
        for value in variants {
            revision = workspace_cli::replace_config(&config, &revision, &value).unwrap();
            assert!(public_status(&config).is_err());
            assert!(manage(
                "invite-computer",
                &config,
                json!({"expected_revision":revision}),
                false
            )
            .await
            .is_err());
            assert_eq!(
                workspace_cli::read_private(&private, ACTIVE)
                    .unwrap()
                    .unwrap(),
                before
            );
        }
    }
    #[tokio::test]
    async fn verified_retirement_does_not_authorize_unverified_process_stop() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("connector.json");
        let private = private_directory(&config).unwrap();
        let store = Store::lock(&private, true).unwrap();
        let mut pending = ready(&config, String::new());
        let profile = pending.profile.as_mut().unwrap();
        save(&store, ACTIVE, profile).unwrap();
        assert!(!is_verified_retired(&private).unwrap());
        let revision = workspace_cli::replace_config(
            &config,
            "",
            &json!({
                "server_url":profile.server_url,"state_dir":profile.state_dir,
                "encrypted_workspace":{"version":1,"state_dir":private},"hgs_path":"hgs"
            }),
        )
        .unwrap();
        crate::fsguard::create_directory(&profile.state_dir).unwrap();
        let journal =
            crate::fsguard::private_file(&profile.state_dir.join(crate::journal::LOCK), true)
                .unwrap()
                .unwrap();
        crate::fsguard::lock_exclusive(&journal).unwrap();
        let generation = crate::workspace_lifecycle::publish(&profile.state_dir).unwrap();
        let identity = Identity::from_record(&profile.identity).unwrap();
        let revoke = SignedMembership::sign(
            MembershipStatement {
                workspace_id: profile.membership.workspace_id.clone(),
                revision: profile.membership.revision + 1,
                previous_head: profile.membership.head.clone(),
                issuer_device_id: identity.public().device_id.clone(),
                issuer_key_generation: identity.public().key_generation,
                action: MembershipAction::Revoke,
                device: None,
                revoked_device_id: Some(identity.public().device_id.clone()),
            },
            &identity,
        )
        .unwrap();
        profile.membership.apply_next(&revoke).unwrap();
        profile
            .historical_heads
            .insert(profile.membership.revision, profile.membership.head.clone());
        save(&store, ACTIVE, profile).unwrap();
        assert!(is_verified_retired(&private).unwrap());
        assert!(load_active(&private).is_err());
        let input = json!({"expected_revision":revision,"generation":generation});
        assert!(crate::workspace_lifecycle::manage_start(&config, &input)
            .await
            .is_err());
        // No verified supervisor or exact connector process owns this fixture
        // lock. Retirement proves neither; refusal must precede a Stop marker.
        assert!(crate::workspace_lifecycle::manage_stop(&config, &input)
            .await
            .is_err());
        assert!(!profile.state_dir.join("workspace-stop.json").exists());
        drop(journal);
        // A corrupt pin is an error, never evidence allowing local shutdown
        // or an implicit reset. HTTP401 does not modify this private record.
        profile.historical_heads.clear();
        save(&store, ACTIVE, profile).unwrap();
        assert!(is_verified_retired(&private).is_err());
    }
}
