//! Endpoint-encrypted relay transport. No v1 private endpoint is used here.
//! The native journal is unchanged; a separate permanent wire identity is
//! committed before native admission and ciphertext is committed before POST.
use crate::{
    execute::{Fatal, Outcome},
    http::Relay,
    journal::JournalError,
    native::{Call, Native},
    runtime::{self, Connector},
    workspace::{self, Profile},
};
use futures_util::future::{self, Either};
use http::Method;
use serde_json::{json, Value};
use std::{
    cell::RefCell, collections::HashSet, future::Future, path::Path, rc::Rc, time::Duration,
};
use zerus_mobile_crypto::{
    self as crypto, EncryptedEnvelope, Header, Identity, OperationClass, PayloadKind, Role,
};
use zerus_relay::alerts;

const MAX_POLL: usize = 41 * 1024 * 1024;
const MAX_SMALL: usize = 2 * 1024 * 1024;
const GUARD_ABI: &str = "Encrypted workspace ABI: guarded-local-v1";
const CLAIM_AUTHORIZATION_WAIT: Duration = Duration::from_secs(60);
const AUTHORIZATION_RETRY: Duration = Duration::from_millis(100);

enum Failure {
    Retry,
    CachePressure,
    Fatal(JournalError),
}
impl Failure {
    fn request_retry_wait(&self, network_delay: &mut u64) -> Option<Duration> {
        match self {
            Self::CachePressure => {
                // Each bounded journal turn commits its safe frontier. Continue
                // that local work at the normal poll cadence, without treating
                // it as a failed network request or spinning without a yield.
                *network_delay = 1;
                Some(Duration::from_millis(250))
            }
            Self::Retry => {
                *network_delay = (*network_delay * 2).min(60);
                Some(Duration::from_secs(*network_delay))
            }
            Self::Fatal(_) => None,
        }
    }
}
impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Encrypted transport refused")
    }
}
type Result<T> = std::result::Result<T, Failure>;
fn checked<T>(value: crypto::Result<T>) -> Result<T> {
    value.map_err(|_| Failure::Retry)
}
fn journal<T>(value: crate::journal::Result<T>) -> Result<T> {
    value.map_err(|error| match error {
        JournalError::Refused(ref refusal) if refusal.message() == crate::error::RETAINED_LIMIT => {
            Failure::CachePressure
        }
        other => Failure::Fatal(other),
    })
}
fn encoded(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| Failure::Retry)
}
async fn call(
    relay: &Relay,
    method: Method,
    path: &str,
    body: Option<Vec<u8>>,
    cap: usize,
) -> Result<Value> {
    relay
        .call(method, path, body, cap)
        .await
        .map_err(|_| Failure::Retry)?
        .ok_or(Failure::Retry)
}

/// Legacy headers were committed only after authenticated/decrypted admission;
/// a2 rejects Read/non-READS before native dispatch. The gateway's full signed
/// Result echoes that accepted class. No fresh request signature is invented:
/// its full envelope is not stored. Unknown producer/key/history stays durable.
fn accepted_read_cache(
    profile: &Profile,
    id: &str,
    digest: &str,
    header: &str,
    result: &str,
) -> bool {
    if !crate::pyvalue::canonical_uuid(id) || crypto::decode_fixed::<32>(digest).is_err() {
        return false;
    }
    let Ok(request) = serde_json::from_str::<Header>(header) else {
        return false;
    };
    let Ok(envelope) = serde_json::from_str::<EncryptedEnvelope>(result) else {
        return false;
    };
    let response = &envelope.header;
    let me = &profile.identity.device;
    let Some(phone) = profile.membership.members.get(&request.sender_device_id) else {
        return false;
    };
    request.canonical_bytes().is_ok()
        && request.kind == PayloadKind::Request
        && response.kind == PayloadKind::Result
        && request.operation_class == OperationClass::Read
        && response.operation_class == OperationClass::Read
        && phone.role == Role::Phone
        && me.role == Role::Computer
        && profile.membership.members.get(&me.device_id) == Some(me)
        && request.workspace_id == profile.membership.workspace_id
        && response.workspace_id == request.workspace_id
        && request.request_id == id
        && response.request_id == id
        && request.recipient_device_id == me.device_id
        && request.recipient_key_generation == me.key_generation
        && request.gateway_id == me.device_id
        && response.gateway_id == request.gateway_id
        && response.sender_device_id == me.device_id
        && response.sender_key_generation == me.key_generation
        && response.recipient_device_id == phone.device_id
        && response.recipient_key_generation == phone.key_generation
        && request.sender_key_generation == phone.key_generation
        && response.route_id == request.route_id
        && profile.historical_heads.get(&request.membership_revision)
            == Some(&request.membership_head)
        && profile.historical_heads.get(&response.membership_revision)
            == Some(&response.membership_head)
        && envelope.verify_sender(me).is_ok()
}

struct Engine<'a, N: Native> {
    connector: &'a Rc<Connector<N>>,
    relay: &'a Relay,
    private_dir: &'a Path,
    profile: RefCell<Rc<Profile>>,
    machine: RefCell<Option<String>>,
    published: RefCell<HashSet<String>>,
    authorization: tokio::sync::Mutex<()>,
}
impl<N: Native + 'static> Engine<'_, N> {
    async fn authorized_profile(
        &self,
    ) -> Result<(
        tokio::sync::MutexGuard<'_, ()>,
        crate::private_state::Store,
        Profile,
    )> {
        self.authorized_profile_before(None).await
    }
    async fn authorization_pause(&self, deadline: tokio::time::Instant) -> Result<()> {
        if self.connector.stopping.is_set() || tokio::time::Instant::now() >= deadline {
            return Err(Failure::Retry);
        }
        let wake = deadline.min(tokio::time::Instant::now() + AUTHORIZATION_RETRY);
        runtime::until_stopped(&self.connector.stopping, tokio::time::sleep_until(wake))
            .await
            .ok_or(Failure::Retry)?;
        if tokio::time::Instant::now() >= deadline {
            return Err(Failure::Retry);
        }
        Ok(())
    }
    async fn authorized_profile_before(
        &self,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<(
        tokio::sync::MutexGuard<'_, ()>,
        crate::private_state::Store,
        Profile,
    )> {
        // Serialize authorization/content release with locally accepted
        // revocations, including those made by a different management process.
        // The asynchronous gate also prevents this runtime's heartbeat from
        // refusing a request it has already claimed while holding the file lock.
        let (local, guard) = loop {
            let local = if let Some(deadline) = deadline {
                if self.connector.stopping.is_set() || tokio::time::Instant::now() >= deadline {
                    return Err(Failure::Retry);
                }
                runtime::until_stopped(
                    &self.connector.stopping,
                    tokio::time::timeout_at(deadline, self.authorization.lock()),
                )
                .await
                .ok_or(Failure::Retry)?
                .map_err(|_| Failure::Retry)?
            } else {
                self.authorization.lock().await
            };
            match crate::private_state::Store::lock(self.private_dir, false) {
                Ok(guard) => break (local, guard),
                Err(_) => {
                    drop(local);
                    if let Some(deadline) = deadline {
                        self.authorization_pause(deadline).await?;
                    } else {
                        return Err(Failure::Retry);
                    }
                }
            }
        };
        let profile = workspace::load_active(self.private_dir).map_err(|_| Failure::Retry)?;
        if deadline.is_some_and(|deadline| {
            self.connector.stopping.is_set() || tokio::time::Instant::now() >= deadline
        }) {
            return Err(Failure::Retry);
        }
        let pinned = self.profile.borrow();
        if profile.identity.device != pinned.identity.device
            || profile.server_url != pinned.server_url
            || profile.membership.workspace_id != pinned.membership.workspace_id
        {
            return Err(Failure::Retry);
        }
        Ok((local, guard, profile))
    }
    async fn refresh_claimed(&self, deadline: tokio::time::Instant) -> Result<Rc<Profile>> {
        loop {
            if self.connector.stopping.is_set() || tokio::time::Instant::now() >= deadline {
                return Err(Failure::Retry);
            }
            let refreshed = runtime::until_stopped(
                &self.connector.stopping,
                tokio::time::timeout_at(deadline, self.refresh()),
            )
            .await
            .ok_or(Failure::Retry)?
            .map_err(|_| Failure::Retry)?;
            match refreshed {
                Ok(profile) => return Ok(profile),
                Err(Failure::Fatal(error)) => return Err(Failure::Fatal(error)),
                Err(Failure::Retry | Failure::CachePressure) => {
                    self.authorization_pause(deadline).await?
                }
            }
        }
    }
    async fn refresh(&self) -> Result<Rc<Profile>> {
        let _local = self.authorization.lock().await;
        let next = match workspace::refresh_and_approve(self.private_dir, self.relay).await {
            Ok(profile) => profile,
            Err(_) => {
                // Only a locally verified signed tombstone ends this generation.
                // A relay authorization error may be transient or malicious.
                if workspace::is_verified_retired(self.private_dir).unwrap_or(false) {
                    self.connector.stopping.set();
                }
                return Err(Failure::Retry);
            }
        };
        let old = self.profile.borrow();
        if next.identity.device != old.identity.device
            || next.server_url != old.server_url
            || next.state_dir != old.state_dir
            || next.membership.workspace_id != old.membership.workspace_id
        {
            self.connector.stopping.set();
            return Err(Failure::Retry);
        }
        drop(old);
        let next = Rc::new(next);
        *self.profile.borrow_mut() = next.clone();
        Ok(next)
    }
    async fn local_identity(&self) -> Result<String> {
        let crate::native::Output::Text(help) = self
            .connector
            .native(Call::new(["--help"]).text())
            .await
            .map_err(|_| Failure::Retry)?
        else {
            return Err(Failure::Retry);
        };
        if !help.contains(GUARD_ABI) {
            return Err(Failure::Retry);
        }
        let identity = self
            .connector
            .native(Call::new(["mobile", "identity", "--json"]))
            .await
            .map_err(|_| Failure::Retry)?
            .json();
        let machine = identity["machine_id"]
            .as_str()
            .filter(|id| crate::pyvalue::canonical_uuid(id))
            .ok_or(Failure::Retry)?
            .to_owned();
        if self
            .machine
            .borrow()
            .as_ref()
            .is_some_and(|prior| prior != &machine)
        {
            self.connector.stopping.set();
            return Err(Failure::Retry);
        }
        *self.machine.borrow_mut() = Some(machine.clone());
        Ok(machine)
    }
    fn header(
        profile: &Profile,
        recipient: &crypto::DevicePublic,
        route: &str,
        id: String,
        kind: PayloadKind,
        operation_class: OperationClass,
        expires_at: u64,
    ) -> Header {
        Header {
            workspace_id: profile.membership.workspace_id.clone(),
            sender_device_id: profile.identity.device.device_id.clone(),
            recipient_device_id: recipient.device_id.clone(),
            sender_key_generation: profile.identity.device.key_generation,
            recipient_key_generation: recipient.key_generation,
            gateway_id: profile.identity.device.device_id.clone(),
            route_id: route.to_owned(),
            request_id: id,
            kind,
            operation_class,
            expires_at,
            membership_revision: profile.membership.revision,
            membership_head: profile.membership.head.clone(),
        }
    }
    async fn snapshot(
        &self,
        profile: &Profile,
        machine: &str,
        route: &str,
        name: &str,
        mut snapshot: Value,
        presence: Value,
    ) -> Result<()> {
        let secret = checked(crypto::decode(&profile.identity.signing_private_key, 32))?;
        let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, &secret);
        let mut signals = Vec::new();
        let mut signals_overflow = false;
        if let Some(sessions) = snapshot["sessions"].as_array_mut() {
            for row in sessions {
                if alerts::archived(row) {
                    continue;
                }
                let scope = crypto::encode(
                    ring::hmac::sign(
                        &key,
                        &encoded(&json!([
                            profile.membership.workspace_id,
                            machine,
                            row["name"],
                            row["run_id"],
                            row["conversation_id"]
                        ]))?,
                    )
                    .as_ref(),
                );
                row["relay_scope_id"] = json!(scope);
                for (kind, candidate_kind) in [
                    ("input", "attention"),
                    ("error", "error"),
                    ("completed", "completed"),
                ] {
                    let candidate = alerts::Candidate {
                        session: String::new(),
                        kind: candidate_kind.into(),
                        run_id: row["run_id"].as_str().map(str::to_owned),
                        conversation_id: row["conversation_id"].as_str().map(str::to_owned),
                        attention_signature: row["attention_signature"].as_str().map(str::to_owned),
                        reply_id: if kind == "completed" {
                            row["reply_id"].as_str().map(str::to_owned)
                        } else {
                            None
                        },
                    };
                    let active = if kind == "completed" {
                        candidate.reply_id.is_some() && !candidate.resolved(Some(row))
                    } else {
                        alerts::native_need(row, kind) && !candidate.resolved(Some(row))
                    };
                    let change = crypto::encode(
                        ring::hmac::sign(
                            &key,
                            &encoded(&json!([
                                scope,
                                kind,
                                if kind == "completed" {
                                    &row["reply_id"]
                                } else {
                                    &row["attention_signature"]
                                }
                            ]))?,
                        )
                        .as_ref(),
                    );
                    if active {
                        if signals.len() == 1000 {
                            signals_overflow = true;
                        } else {
                            signals.push(json!({"scope_id":scope,"route_id":route,"kind":kind,"change_id":change,"active":true}));
                        }
                    }
                }
            }
        }
        let via = if route == profile.identity.device.device_id {
            Value::Null
        } else {
            json!({"gateway_id":self.machine.borrow().as_deref().ok_or(Failure::Retry)?,"gateway_name":profile.computer_name})
        };
        let row = json!({"id":machine,"machine_id":machine,"name":name,"host":snapshot["host"],"online":true,"snapshot":snapshot,"via":via,"encrypted_transport":{"gateway_id":profile.identity.device.device_id,"route_id":route,"machine_id":machine}});
        let plain = encoded(&row)?;
        if plain.len() > 1024 * 1024 {
            return Err(Failure::Retry);
        }
        let now = crate::now() as u64;
        let bucket = now / 300;
        let recipients: Vec<String> = profile
            .membership
            .members
            .values()
            .filter(|d| d.role == Role::Phone)
            .map(|d| d.device_id.clone())
            .collect();
        for recipient_id in recipients {
            let (_local, _authorization, current) = self.authorized_profile().await?;
            let Some(recipient) = current
                .membership
                .members
                .get(&recipient_id)
                .filter(|d| d.role == Role::Phone)
            else {
                continue;
            };
            let identity = checked(Identity::from_record(&current.identity))?;
            let slot = format!("{route}/{}", recipient.device_id);
            let fingerprint = zerus_relay::json::digest(encoded(&json!([
                zerus_relay::json::digest(&plain),
                current.membership.head,
                recipient,
                bucket
            ]))?);
            let envelope = if let Some(retained) = journal(
                self.connector
                    .journal
                    .encrypted_snapshot(&slot, &fingerprint),
            )? {
                retained
            } else {
                let header = Self::header(
                    &current,
                    recipient,
                    route,
                    uuid::Uuid::new_v4().to_string(),
                    PayloadKind::Snapshot,
                    OperationClass::Read,
                    (bucket + 12) * 300,
                );
                let envelope = checked(crypto::seal(&identity, recipient, header, &plain))?;
                let text = String::from_utf8(encoded(&envelope)?).map_err(|_| Failure::Retry)?;
                journal(self.connector.journal.encrypted_save_snapshot(
                    &slot,
                    &fingerprint,
                    &text,
                ))?;
                text
            };
            // Avoid an extra full ciphertext clone while framing the object.
            let mut body = encoded(&json!({"computer_id":machine}))?;
            body.pop();
            body.extend_from_slice(b",\"envelope\":");
            body.extend_from_slice(envelope.as_bytes());
            body.push(b'}');
            if body.len() > MAX_SMALL {
                return Err(Failure::Retry);
            }
            call(
                self.relay,
                Method::POST,
                "/v2/node/snapshot",
                Some(body),
                MAX_SMALL,
            )
            .await?;
        }
        self.published.borrow_mut().insert(route.to_owned());
        let delivery = alerts::Delivery::from_snapshot(&row["snapshot"])
            .unwrap_or(alerts::Delivery::DEFAULT)
            .name();
        // A complete active set also withdraws vanished/archived scopes. Never
        // advertise a truncated set as complete or retain an unbounded cache.
        if signals_overflow {
            return Err(Failure::Retry);
        }
        call(self.relay,Method::POST,"/v2/node/signals",Some(encoded(&json!({"presence":presence,"delivery":delivery,"complete_route":route,"signals":signals}))?),MAX_SMALL).await?;
        Ok(())
    }
    async fn observe_local(&self, machine: &str) -> Result<Value> {
        Connector::<N>::encrypted_guarded(machine.to_owned(), async {
            let snapshot = self
                .connector
                .snapshot()
                .await
                .map_err(|_| Failure::Retry)?;
            self.connector.discover().await;
            Ok(snapshot)
        })
        .await
    }
    async fn heartbeat(&self) -> Result<()> {
        let profile = self.refresh().await?;
        let machine = self.local_identity().await?;
        let snapshot = self.observe_local(&machine).await?;
        let gateway = profile.identity.device.device_id.clone();
        let presence = self.connector.state.borrow().presence.clone();
        let mut manifest = vec![json!({"route_id":gateway,"computer_id":machine})];
        if let Some(fleet) = &self.connector.fleet {
            manifest.extend(
                fleet
                    .routes
                    .borrow()
                    .values()
                    .filter(|r| r.online)
                    .map(|r| json!({"route_id":r.route_id,"computer_id":r.machine_id})),
            );
        }
        call(
            self.relay,
            Method::POST,
            "/v2/node/routes",
            Some(encoded(&json!({"routes":manifest}))?),
            MAX_SMALL,
        )
        .await?;
        self.snapshot(
            &profile,
            &machine,
            &gateway,
            &profile.computer_name,
            snapshot,
            presence,
        )
        .await?;
        let mut current = HashSet::from([gateway]);
        if let Some(fleet) = &self.connector.fleet {
            let routes: Vec<_> = fleet
                .routes
                .borrow()
                .iter()
                .map(|(id, r)| (id.clone(), r.clone()))
                .collect();
            for (id, route) in routes {
                if !route.online {
                    continue;
                }
                let Some(context) = fleet.contexts.borrow().get(&id).cloned() else {
                    continue;
                };
                let Ok(snapshot) = context.snapshot().await else {
                    continue;
                };
                if fleet.routes.borrow().get(&id) != Some(&route) {
                    continue;
                }
                let presence = context.state.borrow().presence.clone();
                self.snapshot(
                    &profile,
                    &route.machine_id,
                    &id,
                    &route.name,
                    snapshot,
                    presence,
                )
                .await?;
                current.insert(id);
            }
        }
        let old: Vec<_> = self
            .published
            .borrow()
            .difference(&current)
            .cloned()
            .collect();
        for route in old {
            call(
                self.relay,
                Method::DELETE,
                &format!("/v2/node/routes/{route}"),
                None,
                MAX_SMALL,
            )
            .await?;
            self.published.borrow_mut().remove(&route);
        }
        let slots: HashSet<_> = current
            .iter()
            .flat_map(|r| {
                profile
                    .membership
                    .members
                    .values()
                    .filter(|d| d.role == Role::Phone)
                    .map(move |d| format!("{r}/{}", d.device_id))
            })
            .collect();
        journal(self.connector.journal.encrypted_drop_snapshots(&slots))?;
        Ok(())
    }
    async fn heartbeats(&self) -> Result<()> {
        loop {
            match self.heartbeat().await {
                Ok(()) => {}
                Err(Failure::Fatal(e)) => return Err(Failure::Fatal(e)),
                Err(Failure::Retry | Failure::CachePressure) => runtime::log(
                    "WARNING",
                    "Encrypted workspace refresh is unavailable; private transport remains closed",
                ),
            }
            if runtime::until_stopped(
                &self.connector.stopping,
                tokio::time::sleep(Duration::from_secs_f64(self.connector.settings.interval)),
            )
            .await
            .is_none()
            {
                return Ok(());
            }
        }
    }
    fn seal_receipt(
        &self,
        profile: &Profile,
        request: &Header,
        mut receipt: Value,
    ) -> Result<String> {
        let recipient = profile
            .membership
            .members
            .get(&request.sender_device_id)
            .filter(|d| d.role == Role::Phone && d.key_generation == request.sender_key_generation)
            .ok_or(Failure::Retry)?;
        receipt["request_id"] = json!(request.request_id);
        let mut header = Self::header(
            profile,
            recipient,
            &request.route_id,
            request.request_id.clone(),
            PayloadKind::Result,
            request.operation_class,
            crate::now() as u64 + 23 * 3600,
        );
        header.gateway_id = request.gateway_id.clone();
        let mut private = encoded(&receipt)?;
        if private.len() > 1024 * 1024 {
            private = encoded(
                &json!({"request_id":request.request_id,"state":"uncertain","result":null,"error":"Native work may have completed, but its result exceeds the encrypted relay limit; this request will not execute again"}),
            )?;
        }
        let envelope = checked(crypto::seal(
            &checked(Identity::from_record(&profile.identity))?,
            recipient,
            header,
            &private,
        ))?;
        let text = String::from_utf8(encoded(&envelope)?).map_err(|_| Failure::Retry)?;
        if text.len() > MAX_SMALL - 4096 {
            return Err(Failure::Retry);
        }
        journal(self.connector.journal.encrypted_reclaim_read_cache(
            crate::now(),
            text.len(),
            |id, digest, header, result| accepted_read_cache(profile, id, digest, header, result),
        ))?;
        journal(
            self.connector
                .journal
                .encrypted_result(&request.request_id, &text, crate::now()),
        )?;
        Ok(text)
    }
    async fn flush(&self, _profile: &Profile) -> Result<()> {
        journal(self.connector.journal.encrypted_maintenance(crate::now()))?;
        for (id, header, envelope) in journal(self.connector.journal.encrypted_pending())? {
            let (_local, _authorization, profile) = self.authorized_profile().await?;
            let header: Header = serde_json::from_str(&header).map_err(|_| Failure::Retry)?;
            if !profile
                .membership
                .members
                .contains_key(&header.sender_device_id)
            {
                journal(
                    self.connector
                        .journal
                        .encrypted_delivered(&id, crate::now()),
                )?;
                continue;
            }
            let envelope = if let Some(envelope) = envelope {
                envelope
            } else {
                let receipt=journal(self.connector.journal.encrypted_native_receipt(&id))?.and_then(|text|serde_json::from_str(&text).ok()).unwrap_or_else(||crate::request::receipt("uncertain",Value::Null,Some("Connector stopped before recording this encrypted request; it will not execute again")));
                self.seal_receipt(&profile, &header, receipt)?
            };
            let mut body = b"{\"envelope\":".to_vec();
            body.extend_from_slice(envelope.as_bytes());
            body.push(b'}');
            match self
                .relay
                .call(
                    Method::POST,
                    &format!("/v2/node/requests/{id}/result"),
                    Some(body),
                    MAX_SMALL,
                )
                .await
            {
                Ok(_) => {}
                Err(error) if matches!(error.status(), Some(404 | 409 | 410)) => {}
                Err(_) => return Err(Failure::Retry),
            }
            journal(
                self.connector
                    .journal
                    .encrypted_delivered(&id, crate::now()),
            )?;
        }
        Ok(())
    }
    #[cfg(test)]
    async fn receive(&self, profile: &Profile, value: Value) -> Result<()> {
        self.receive_before(
            profile,
            value,
            tokio::time::Instant::now() + CLAIM_AUTHORIZATION_WAIT,
        )
        .await
    }
    async fn receive_before(
        &self,
        _profile: &Profile,
        value: Value,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        // Only authorization acquisition may wait/cancel. Once the permanent
        // identity is reserved, native execution must finish without replay.
        let (_local, _authorization, profile) =
            self.authorized_profile_before(Some(deadline)).await?;
        let envelope: EncryptedEnvelope =
            serde_json::from_value(value).map_err(|_| Failure::Retry)?;
        let h = &envelope.header;
        let me = &profile.identity.device;
        if h.kind != PayloadKind::Request
            || h.recipient_device_id != me.device_id
            || h.gateway_id != me.device_id
            || h.expires_at <= crate::now() as u64
            || !crate::pyvalue::canonical_uuid(&h.request_id)
        {
            return Err(Failure::Retry);
        }
        let head = profile
            .historical_heads
            .get(&h.membership_revision)
            .ok_or(Failure::Retry)?;
        let sender = checked(
            profile
                .membership
                .authorized_sender_at_known_head(&envelope, head),
        )?;
        if sender.role != Role::Phone {
            return Err(Failure::Retry);
        }
        let private = checked(crypto::open(
            &checked(Identity::from_record(&profile.identity))?,
            sender,
            h,
            &envelope,
        ))?;
        let mut body = crate::request::parse(&private, 64, 100_000).map_err(|_| Failure::Retry)?;
        let binding = &body["encrypted_transport"];
        let machine = binding["machine_id"]
            .as_str()
            .filter(|m| crate::pyvalue::canonical_uuid(m))
            .ok_or(Failure::Retry)?
            .to_owned();
        if body["request_id"] != h.request_id
            || body["computer_id"] != machine
            || binding["gateway_id"] != h.gateway_id
            || binding["route_id"] != h.route_id
            || body.get("gateway_route").is_some()
        {
            return Err(Failure::Retry);
        }
        let object = body.as_object_mut().ok_or(Failure::Retry)?;
        object.remove("encrypted_transport");
        object.insert("expires_at".into(), json!(h.expires_at));
        let local = h.route_id == me.device_id;
        if local {
            if self.machine.borrow().as_deref() != Some(machine.as_str()) {
                return Err(Failure::Retry);
            }
        } else {
            let fleet = self.connector.fleet.as_ref().ok_or(Failure::Retry)?;
            if !fleet
                .routes
                .borrow()
                .get(&h.route_id)
                .is_some_and(|r| r.online && r.machine_id == machine)
            {
                return Err(Failure::Retry);
            }
            object.insert("gateway_route".into(),json!({"schema":1,"route_id":h.route_id,"computer_id":machine,"machine_id":machine}));
        }
        let digest = checked(envelope.digest())?;
        let header = String::from_utf8(encoded(h)?).map_err(|_| Failure::Retry)?;
        if !journal(self.connector.journal.encrypted_reserve(
            &h.request_id,
            &digest,
            &header,
            crate::now(),
        ))? {
            return Ok(());
        }
        let operation = body["operation"].as_str().unwrap_or("");
        if h.operation_class == OperationClass::Control
            || (h.operation_class == OperationClass::Read
                && !zerus_relay::protocol::READS.contains(&operation))
        {
            self.seal_receipt(
                &profile,
                h,
                crate::request::receipt(
                    "failed",
                    Value::Null,
                    Some("Encrypted request class does not match the native operation"),
                ),
            )?;
            return Ok(());
        }
        let outcome = if local {
            Connector::<N>::encrypted_guarded(machine, self.connector.execute(body)).await
        } else {
            self.connector.execute(body).await
        };
        match outcome {
            Ok(Outcome::Rejected(error)) => {
                self.seal_receipt(
                    &profile,
                    h,
                    crate::request::receipt("failed", Value::Null, Some(&error)),
                )?;
            }
            Ok(_) => {}
            Err(Fatal(error)) => return Err(Failure::Fatal(error)),
        }
        Ok(())
    }
    async fn requests(&self) -> Result<()> {
        if runtime::until_stopped(
            &self.connector.stopping,
            self.connector.snapshot_ready.wait(),
        )
        .await
        .is_none()
        {
            return Ok(());
        }
        let mut delay = 1;
        loop {
            let iteration = async {
                let profile = self.refresh().await?;
                self.flush(&profile).await?;
                let Some(batch) = runtime::until_stopped(
                    &self.connector.stopping,
                    call(
                        self.relay,
                        Method::GET,
                        "/v2/node/requests?wait=25",
                        None,
                        MAX_POLL,
                    ),
                )
                .await
                else {
                    return Ok(false);
                };
                let mut batch = batch?;
                let Value::Array(rows) = batch["requests"].take() else {
                    return Err(Failure::Retry);
                };
                if rows.len() > 1 {
                    return Err(Failure::Retry);
                }
                let received = !rows.is_empty();
                let deadline = tokio::time::Instant::now() + CLAIM_AUTHORIZATION_WAIT;
                let profile = self.refresh_claimed(deadline).await?;
                for row in rows {
                    if self.connector.stopping.is_set() {
                        return Ok(false);
                    }
                    self.receive_before(&profile, row, deadline).await?;
                }
                if self.connector.stopping.is_set() {
                    return Ok(false);
                }
                self.flush(&profile).await?;
                Ok(received)
            };
            let wait = match iteration.await {
                Ok(received) => {
                    delay = 1;
                    // A completed turn has already flushed its durable result.
                    // Yield to heartbeat/stop, then claim the next queued ID
                    // without adding a quarter second to every read. Empty
                    // polls retain their pacing; quota/error waits are separate.
                    if received {
                        Duration::ZERO
                    } else {
                        Duration::from_millis(250)
                    }
                }
                Err(error) => {
                    let Some(wait) = error.request_retry_wait(&mut delay) else {
                        return Err(error);
                    };
                    runtime::log("WARNING","Encrypted request delivery is unavailable; stored identities are preserved");
                    wait
                }
            };
            if self.connector.stopping.is_set() {
                return Ok(());
            }
            if runtime::until_stopped(&self.connector.stopping, tokio::time::sleep(wait))
                .await
                .is_none()
            {
                return Ok(());
            }
        }
    }
}

pub async fn run<N: Native + 'static>(
    connector: &Rc<Connector<N>>,
    relay: &Relay,
    private_dir: &Path,
    signals: impl Future<Output = ()>,
) -> u8 {
    let Ok(profile) = workspace::load_active(private_dir) else {
        return 1;
    };
    if connector.journal.encrypted_init(crate::now()).is_err() {
        return 1;
    }
    let Ok(generation) = crate::workspace_lifecycle::publish(&profile.state_dir) else {
        return 1;
    };
    let state_dir = profile.state_dir.clone();
    let engine = Engine {
        connector,
        relay,
        private_dir,
        profile: RefCell::new(Rc::new(profile)),
        machine: RefCell::new(None),
        published: RefCell::default(),
        authorization: tokio::sync::Mutex::new(()),
    };
    let requests = async {
        let result = engine.requests().await;
        connector.stopping.set();
        result
    };
    let heartbeat = runtime::until_stopped(&connector.stopping, async {
        let result = engine.heartbeats().await;
        connector.stopping.set();
        result
    });
    let work = future::join(heartbeat, requests);
    let monitors = async {
        let signal = async {
            signals.await;
            connector.stopping.set();
        };
        let control =
            crate::workspace_lifecycle::monitor(&connector.stopping, &state_dir, &generation);
        let _ = future::select(std::pin::pin!(signal), std::pin::pin!(control)).await;
        connector.stopping.set();
    };
    let (heartbeat, requests) =
        match future::select(std::pin::pin!(work), std::pin::pin!(monitors)).await {
            Either::Left((result, _)) => result,
            // Stop closes admission and cancels polling. It must not drop a
            // native mutation or release its journal before its outcome settles.
            Either::Right(((), work)) => work.await,
        };
    match (heartbeat, requests) {
        (Some(Err(Failure::Fatal(_))), _) | (_, Err(Failure::Fatal(_))) => {
            runtime::log("ERROR", crate::JOURNAL_FAILURE);
            1
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        journal::Journal,
        native::{NativeError, NativeResult, Output},
        private_state::Store,
        runtime::Settings,
    };
    use crypto::{MembershipAction, MembershipState, MembershipStatement, SignedMembership};
    use std::collections::BTreeMap;
    const MACHINE: &str = "00000000-0000-4000-8000-000000000020";
    const REQUEST: &str = "00000000-0000-4000-8000-000000000021";
    // A gateway on Windows always runs as a Windows gateway, which checks the
    // target session with one more inspect before it dispatches a call.
    const DISPATCH_CALLS: usize = if cfg!(windows) { 2 } else { 1 };
    #[derive(Default)]
    struct NativeFixture {
        calls: RefCell<Vec<Vec<String>>>,
        observation: RefCell<Option<Observation>>,
    }
    struct Observation {
        machine: String,
        change_after_listing: bool,
        inventory_reads: usize,
    }
    impl Native for NativeFixture {
        async fn call(&self, call: Call) -> NativeResult {
            self.calls.borrow_mut().push(call.argv.clone());
            if let Some(observation) = self.observation.borrow_mut().as_mut() {
                let argv = if call.argv.first().map(String::as_str) == Some("--expect-machine") {
                    if call.argv.get(1) != Some(&observation.machine) {
                        return Err(NativeError::Failed(crate::fleet::LOCAL_CHANGED));
                    }
                    &call.argv[2..]
                } else {
                    &call.argv[..]
                };
                return match argv
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    ["--help"] => Ok(Output::Text(format!(
                        "{GUARD_ABI}\nmobile-peers --json\nmobile-peer --json\n"
                    ))),
                    ["mobile", "identity", "--json"] => {
                        Ok(Output::Json(json!({"machine_id": observation.machine})))
                    }
                    ["ls", "--json", "--local"] => {
                        if observation.change_after_listing {
                            observation.machine = "00000000-0000-4000-8000-000000000099".into();
                        }
                        Ok(Output::Json(json!({"sessions": []})))
                    }
                    ["swarm", "get"] => Ok(Output::Json(json!({"schema": 1,
                        "initialized": false, "organization": {"projects": []}}))),
                    ["account", "ls"] => Ok(Output::Json(json!({"profiles": []}))),
                    ["swarm", "mobile-peers", "--json"] => {
                        observation.inventory_reads += 1;
                        Ok(Output::Json(json!({"schema": 1,
                            "local": {"machine_id": observation.machine}, "peers": []})))
                    }
                    _ => Err(NativeError::Failed(crate::native::FAILED)),
                };
            }
            Ok(Output::Json(
                json!({"name":"codex/fixture/main","agent":"codex","private":"private native output"}),
            ))
        }
    }
    struct Fixture {
        connector: Rc<Connector<NativeFixture>>,
        phone: Identity,
        private_dir: std::path::PathBuf,
        relay: Relay,
        _temporary: tempfile::TempDir,
    }
    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let private_dir = temporary.path().join("workspace");
            let computer = Identity::generate(
                "00000000-0000-4000-8000-000000000001".into(),
                Role::Computer,
            )
            .unwrap();
            let phone =
                Identity::generate("00000000-0000-4000-8000-000000000002".into(), Role::Phone)
                    .unwrap();
            let genesis = SignedMembership::sign(
                MembershipStatement {
                    workspace_id: "00000000-0000-4000-8000-000000000003".into(),
                    revision: 0,
                    previous_head: crypto::encode(&[0; 32]),
                    issuer_device_id: computer.public().device_id.clone(),
                    issuer_key_generation: 1,
                    action: MembershipAction::Add,
                    device: Some(computer.public().clone()),
                    revoked_device_id: None,
                },
                &computer,
            )
            .unwrap();
            let mut membership = MembershipState::from_genesis(&genesis).unwrap();
            let mut historical_heads = BTreeMap::from([(0, membership.head.clone())]);
            membership
                .apply_next(
                    &SignedMembership::sign(
                        MembershipStatement {
                            workspace_id: membership.workspace_id.clone(),
                            revision: 1,
                            previous_head: membership.head.clone(),
                            issuer_device_id: computer.public().device_id.clone(),
                            issuer_key_generation: 1,
                            action: MembershipAction::Add,
                            device: Some(phone.public().clone()),
                            revoked_device_id: None,
                        },
                        &computer,
                    )
                    .unwrap(),
                )
                .unwrap();
            historical_heads.insert(1, membership.head.clone());
            let profile = Profile {
                server_url: "http://127.0.0.1:1".into(),
                workspace_name: "Private workspace".into(),
                computer_name: "Private computer".into(),
                node_token: crypto::encode(&[1; 32]),
                identity: computer.to_record(),
                membership,
                historical_heads,
                state_dir: temporary.path().join("receipts"),
            };
            let store = Store::lock(&private_dir, true).unwrap();
            store
                .replace("active.json", "", &serde_json::to_vec(&profile).unwrap())
                .unwrap();
            drop(store);
            let relay = workspace::relay(&profile, true).unwrap();
            let journal =
                Journal::open(&profile.state_dir, "encrypted-test", crate::now()).unwrap();
            journal.encrypted_init(crate::now()).unwrap();
            let connector = Rc::new(Connector::new(
                NativeFixture::default(),
                journal,
                Settings {
                    interval: 1.0,
                    binding: "encrypted-test".into(),
                    default_timeout: 1.0,
                    clock_scale: 1.0,
                },
                Box::new(crate::now),
            ));
            connector
                .state
                .borrow_mut()
                .sessions
                .insert("codex/fixture/main".into());
            Self {
                connector,
                phone,
                private_dir,
                relay,
                _temporary: temporary,
            }
        }
        fn engine(&self) -> Engine<'_, NativeFixture> {
            Engine {
                connector: &self.connector,
                relay: &self.relay,
                private_dir: &self.private_dir,
                profile: RefCell::new(Rc::new(workspace::load_active(&self.private_dir).unwrap())),
                machine: RefCell::new(Some(MACHINE.into())),
                published: RefCell::default(),
                authorization: tokio::sync::Mutex::new(()),
            }
        }
        fn request(
            &self,
            profile: &Profile,
            id: &str,
            route: &str,
            extra: &str,
        ) -> EncryptedEnvelope {
            let me = &profile.identity.device;
            let h = Header {
                workspace_id: profile.membership.workspace_id.clone(),
                sender_device_id: self.phone.public().device_id.clone(),
                recipient_device_id: me.device_id.clone(),
                sender_key_generation: 1,
                recipient_key_generation: 1,
                gateway_id: me.device_id.clone(),
                route_id: route.into(),
                request_id: id.into(),
                kind: PayloadKind::Request,
                operation_class: OperationClass::Read,
                expires_at: crate::now() as u64 + 300,
                membership_revision: profile.membership.revision,
                membership_head: profile.membership.head.clone(),
            };
            crypto::seal(&self.phone,me,h,&serde_json::to_vec(&json!({"request_id":id,"computer_id":MACHINE,"session":"codex/fixture/main","operation":"inspect","private_extra":extra,"encrypted_transport":{"gateway_id":me.device_id,"route_id":route,"machine_id":MACHINE}})).unwrap()).unwrap()
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn protected_observation_guards_snapshot_and_direct_peer_discovery() {
        let f = Fixture::new();
        *f.connector.native.observation.borrow_mut() = Some(Observation {
            machine: MACHINE.into(),
            change_after_listing: false,
            inventory_reads: 0,
        });
        let engine = f.engine();
        engine.observe_local(MACHINE).await.unwrap();
        let calls = f.connector.native.calls.borrow();
        assert!(calls.iter().all(|argv| {
            argv.starts_with(&["--expect-machine".to_owned(), MACHINE.to_owned()])
        }));
        assert_eq!(
            calls.last().unwrap(),
            &[
                "--expect-machine",
                MACHINE,
                "swarm",
                "mobile-peers",
                "--json"
            ]
        );
        assert_eq!(
            f.connector
                .fleet
                .as_ref()
                .unwrap()
                .local_id
                .borrow()
                .as_deref(),
            Some(MACHINE)
        );
        assert_eq!(
            f.connector
                .native
                .observation
                .borrow()
                .as_ref()
                .unwrap()
                .inventory_reads,
            1
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn discovery_refuses_a_machine_changed_after_the_guarded_listing() {
        let f = Fixture::new();
        *f.connector.native.observation.borrow_mut() = Some(Observation {
            machine: MACHINE.into(),
            change_after_listing: true,
            inventory_reads: 0,
        });
        let engine = f.engine();
        engine.observe_local(MACHINE).await.unwrap();
        assert_eq!(
            f.connector.native.calls.borrow().last().unwrap(),
            &[
                "--expect-machine",
                MACHINE,
                "swarm",
                "mobile-peers",
                "--json"
            ]
        );
        assert_eq!(
            f.connector
                .native
                .observation
                .borrow()
                .as_ref()
                .unwrap()
                .inventory_reads,
            0
        );
        let fleet = f.connector.fleet.as_ref().unwrap();
        assert!(fleet.local_id.borrow().is_none());
        assert!(fleet.routes.borrow().is_empty());
        // A fresh identity probe verifies the change and stops this generation.
        assert!(engine.local_identity().await.is_err());
        assert!(f.connector.stopping.is_set());
    }
    #[tokio::test]
    async fn authenticated_native_dispatch_is_guarded_and_every_read_identity_is_permanent() {
        let f = Fixture::new();
        let engine = f.engine();
        let profile = workspace::load_active(&f.private_dir).unwrap();
        let route = &profile.identity.device.device_id;
        let envelope = f.request(&profile, REQUEST, route, "private message");
        let value = serde_json::to_value(&envelope).unwrap();
        engine.receive(&profile, value.clone()).await.unwrap();
        engine.receive(&profile, value).await.unwrap();
        assert_eq!(f.connector.native.calls.borrow().len(), DISPATCH_CALLS);
        assert_eq!(
            f.connector.native.calls.borrow().last().unwrap(),
            &["--expect-machine", MACHINE, "inspect", "codex/fixture/main"]
        );
        if cfg!(windows) {
            assert_eq!(
                f.connector.native.calls.borrow()[0],
                [
                    "--expect-machine",
                    MACHINE,
                    "inspect",
                    "codex/fixture/main",
                    "--skip-processes"
                ]
            );
        }
        let native = f
            .connector
            .journal
            .encrypted_native_receipt(REQUEST)
            .unwrap()
            .unwrap();
        let receipt: Value = serde_json::from_str(&native).unwrap();
        assert_eq!(receipt["state"], "completed");
        let ciphertext = engine
            .seal_receipt(&profile, &envelope.header, receipt)
            .unwrap();
        assert!(!ciphertext.contains("private native output"));
        let result: EncryptedEnvelope = serde_json::from_str(&ciphertext).unwrap();
        let opened =
            crypto::open(&f.phone, &profile.identity.device, &result.header, &result).unwrap();
        let decoded: Value = serde_json::from_slice(&opened).unwrap();
        assert_eq!(decoded["result"]["private"], "private native output");
        assert_eq!(decoded["request_id"], REQUEST);
        let changed = f.request(&profile, REQUEST, route, "changed private message");
        assert!(engine
            .receive(&profile, serde_json::to_value(changed).unwrap())
            .await
            .is_err());
        assert_eq!(f.connector.native.calls.borrow().len(), DISPATCH_CALLS);
    }
    #[tokio::test(start_paused = true)]
    async fn queued_authenticated_reads_claim_again_after_flush_without_a_success_sleep() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::time::Instant;

        let mut f = Fixture::new();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut profile = workspace::load_active(&f.private_dir).unwrap();
        profile.server_url = format!("http://{}", listener.local_addr().unwrap());
        let store = Store::lock(&f.private_dir, false).unwrap();
        let old = store.read("active.json").unwrap().unwrap();
        store
            .replace(
                "active.json",
                &Store::revision(Some(&old)),
                &serde_json::to_vec(&profile).unwrap(),
            )
            .unwrap();
        drop(store);
        f.relay = workspace::relay(&profile, true).unwrap();
        let first = serde_json::to_value(f.request(
            &profile,
            REQUEST,
            &profile.identity.device.device_id,
            "",
        ))
        .unwrap();
        let second_id = "00000000-0000-4000-8000-000000000022";
        let second = serde_json::to_value(f.request(
            &profile,
            second_id,
            &profile.identity.device.device_id,
            "",
        ))
        .unwrap();
        let membership = json!({"changes":[],"membership_revision":profile.membership.revision,"membership_head":profile.membership.head,"next_after":null});
        let (finished, stop) = tokio::sync::oneshot::channel();
        // A blocking task inhibits Tokio's paused-clock auto-advance while the
        // real socket fixture is alive. A success sleep can never complete;
        // only the real-time fixture deadline bounds a stalled regression.
        let server = tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut claims = 0;
            let mut results = 0;
            let mut events = Vec::new();
            while Instant::now() < deadline {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                };
                // Accepted sockets may inherit the listener's nonblocking mode on BSD.
                // The bounded fixture reader below deliberately uses blocking reads.
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = Vec::new();
                let body_start = loop {
                    let mut block = [0; 4096];
                    let n = stream.read(&mut block).unwrap();
                    assert!(n > 0 && bytes.len() + n < 2 * 1024 * 1024);
                    bytes.extend_from_slice(&block[..n]);
                    if let Some(i) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let header = String::from_utf8(bytes[..body_start].to_vec()).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while bytes.len() < body_start + length {
                    let mut block = [0; 4096];
                    let n = stream.read(&mut block).unwrap();
                    assert!(n > 0 && bytes.len() + n < 2 * 1024 * 1024);
                    bytes.extend_from_slice(&block[..n]);
                }
                let path = header
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let response = if path.starts_with("/v2/node/membership?") {
                    membership.clone()
                } else if path == "/v2/node/requests?wait=25" {
                    claims += 1;
                    if events.last() == Some(&"result") {
                        // Exceed the former wall-clock assertion deliberately.
                        // Scheduler/socket latency must not look like a success sleep.
                        std::thread::sleep(Duration::from_millis(350));
                    }
                    events.push("claim");
                    match claims {
                        1 => json!({"requests":[first.clone()]}),
                        2 => json!({"requests":[second.clone()]}),
                        3 => json!({"requests":[first.clone()]}), // Same ID must never execute twice.
                        4 => json!({"requests":[]}),
                        _ => panic!("unexpected fixture claim"),
                    }
                } else {
                    assert!(path.starts_with("/v2/node/requests/") && path.ends_with("/result"));
                    results += 1;
                    events.push("result");
                    json!({})
                };
                let response = serde_json::to_vec(&response).unwrap();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n", response.len()).unwrap();
                stream.write_all(&response).unwrap();
                if claims == 4 {
                    break;
                }
            }
            let observed_at = tokio::time::Instant::now();
            finished.send(()).unwrap();
            (claims, results, events, observed_at)
        });
        let clock = tokio::time::Instant::now();
        f.connector.snapshot_ready.set();
        let engine = f.engine();
        let stopping = async {
            stop.await.unwrap();
            f.connector.stopping.set();
        };
        let (result, ()) = future::join(engine.requests(), stopping).await;
        result.unwrap();
        let (claims, results, events, observed_at) = server.await.unwrap();
        assert_eq!(
            observed_at, clock,
            "claims after flushed results must complete without advancing Tokio time"
        );
        assert_eq!(
            (claims, results, f.connector.native.calls.borrow().len()),
            (4, 2, 2 * DISPATCH_CALLS)
        );
        assert_eq!(
            events,
            ["claim", "result", "claim", "result", "claim", "claim"],
            "each result must be flushed before the next claim; a duplicate must not execute"
        );
        assert!(f.connector.journal.encrypted_pending().unwrap().is_empty());
    }
    #[tokio::test]
    async fn own_process_authorization_contention_waits_without_losing_claimed_request() {
        let f = Fixture::new();
        let engine = f.engine();
        let profile = workspace::load_active(&f.private_dir).unwrap();
        let envelope = f.request(&profile, REQUEST, &profile.identity.device.device_id, "");
        let value = serde_json::to_value(envelope).unwrap();
        let authorization = engine.authorization.lock().await;
        let mut receiving = Box::pin(engine.receive(&profile, value.clone()));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), receiving.as_mut())
                .await
                .is_err()
        );
        assert!(f.connector.native.calls.borrow().is_empty());
        assert!(f.connector.journal.encrypted_pending().unwrap().is_empty());
        drop(authorization);
        tokio::time::timeout(Duration::from_secs(1), receiving)
            .await
            .expect("The same claimed request must resume after the local gate opens")
            .unwrap();
        assert_eq!(f.connector.native.calls.borrow().len(), DISPATCH_CALLS);
        let receipt: Value = serde_json::from_str(
            &f.connector
                .journal
                .encrypted_native_receipt(REQUEST)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["state"], "completed");
        engine.receive(&profile, value).await.unwrap();
        assert_eq!(f.connector.native.calls.borrow().len(), DISPATCH_CALLS);
    }
    #[tokio::test]
    async fn external_management_lock_retains_one_receive_and_rechecks_current_authorization() {
        let f = Fixture::new();
        let engine = f.engine();
        let profile = workspace::load_active(&f.private_dir).unwrap();
        let envelope = f.request(&profile, REQUEST, &profile.identity.device.device_id, "");
        let value = serde_json::to_value(envelope).unwrap();
        let management = Store::lock(&f.private_dir, false).unwrap();
        let mut receiving = Box::pin(engine.receive(&profile, value.clone()));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), receiving.as_mut())
                .await
                .is_err()
        );
        assert!(f.connector.native.calls.borrow().is_empty());
        assert!(f.connector.journal.encrypted_pending().unwrap().is_empty());
        assert!(
            engine.authorization.try_lock().is_ok(),
            "Retry pauses must release the local gate"
        );
        drop(management);
        tokio::time::timeout(Duration::from_secs(1), receiving)
            .await
            .expect("The retained request must resume after management unlocks")
            .unwrap();
        engine.receive(&profile, value).await.unwrap();
        assert_eq!(f.connector.native.calls.borrow().len(), DISPATCH_CALLS);

        let next_id = "00000000-0000-4000-8000-000000000022";
        let next = f.request(&profile, next_id, &profile.identity.device.device_id, "");
        let management = Store::lock(&f.private_dir, false).unwrap();
        let mut receiving = Box::pin(engine.receive(&profile, serde_json::to_value(next).unwrap()));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), receiving.as_mut())
                .await
                .is_err()
        );
        let computer = Identity::from_record(&profile.identity).unwrap();
        let revoke = SignedMembership::sign(
            MembershipStatement {
                workspace_id: profile.membership.workspace_id.clone(),
                revision: profile.membership.revision + 1,
                previous_head: profile.membership.head.clone(),
                issuer_device_id: computer.public().device_id.clone(),
                issuer_key_generation: computer.public().key_generation,
                action: MembershipAction::Revoke,
                device: None,
                revoked_device_id: Some(f.phone.public().device_id.clone()),
            },
            &computer,
        )
        .unwrap();
        // Update the private record while the original receive future still
        // waits. It must load this state rather than its old in-memory roster.
        let mut removed = workspace::load_active(&f.private_dir).unwrap();
        removed.membership.apply_next(&revoke).unwrap();
        removed
            .historical_heads
            .insert(removed.membership.revision, removed.membership.head.clone());
        let revision = Store::revision(management.read("active.json").unwrap().as_deref());
        management
            .replace(
                "active.json",
                &revision,
                &serde_json::to_vec(&removed).unwrap(),
            )
            .unwrap();
        drop(management);
        assert!(tokio::time::timeout(Duration::from_secs(1), receiving)
            .await
            .expect("Revoked authorization must fail after management unlocks")
            .is_err());
        assert_eq!(f.connector.native.calls.borrow().len(), DISPATCH_CALLS);
        assert!(f
            .connector
            .journal
            .encrypted_native_receipt(next_id)
            .unwrap()
            .is_none());
    }
    #[tokio::test]
    async fn wrong_route_forgery_and_locally_accepted_revoke_never_reach_native() {
        let f = Fixture::new();
        let engine = f.engine();
        let mut profile = workspace::load_active(&f.private_dir).unwrap();
        let wrong = f.request(
            &profile,
            REQUEST,
            "00000000-0000-4000-8000-000000000099",
            "",
        );
        assert!(engine
            .receive(&profile, serde_json::to_value(wrong).unwrap())
            .await
            .is_err());
        let valid = f.request(&profile, REQUEST, &profile.identity.device.device_id, "");
        let mut forged = serde_json::to_value(&valid).unwrap();
        forged["signature"] = json!(crypto::encode(&[0; 64]));
        assert!(engine.receive(&profile, forged).await.is_err());
        let computer = Identity::from_record(&profile.identity).unwrap();
        let revoke = SignedMembership::sign(
            MembershipStatement {
                workspace_id: profile.membership.workspace_id.clone(),
                revision: 2,
                previous_head: profile.membership.head.clone(),
                issuer_device_id: computer.public().device_id.clone(),
                issuer_key_generation: 1,
                action: MembershipAction::Revoke,
                device: None,
                revoked_device_id: Some(f.phone.public().device_id.clone()),
            },
            &computer,
        )
        .unwrap();
        profile.membership.apply_next(&revoke).unwrap();
        profile
            .historical_heads
            .insert(2, profile.membership.head.clone());
        let store = Store::lock(&f.private_dir, false).unwrap();
        let revision = Store::revision(store.read("active.json").unwrap().as_deref());
        store
            .replace(
                "active.json",
                &revision,
                &serde_json::to_vec(&profile).unwrap(),
            )
            .unwrap();
        drop(store);
        // Engine still has its previous in-memory roster. Authorization must
        // consult the durable head under the same lock as local removal.
        assert!(engine
            .receive(&profile, serde_json::to_value(valid).unwrap())
            .await
            .is_err());
        assert!(f.connector.native.calls.borrow().is_empty());
    }
    #[tokio::test]
    async fn mismatched_service_class_is_receipted_once_without_native_execution() {
        let f = Fixture::new();
        let engine = f.engine();
        let profile = workspace::load_active(&f.private_dir).unwrap();
        let valid = f.request(&profile, REQUEST, &profile.identity.device.device_id, "");
        let mut body: Value = serde_json::from_slice(
            &crypto::open(
                &Identity::from_record(&profile.identity).unwrap(),
                f.phone.public(),
                &valid.header,
                &valid,
            )
            .unwrap(),
        )
        .unwrap();
        body["operation"] = json!("send");
        let mismatched = crypto::seal(
            &f.phone,
            &profile.identity.device,
            valid.header,
            &serde_json::to_vec(&body).unwrap(),
        )
        .unwrap();
        let value = serde_json::to_value(mismatched).unwrap();
        engine.receive(&profile, value.clone()).await.unwrap();
        engine.receive(&profile, value).await.unwrap();
        let pending = f.connector.journal.encrypted_pending().unwrap();
        let result: EncryptedEnvelope =
            serde_json::from_str(pending[0].2.as_deref().unwrap()).unwrap();
        let plain =
            crypto::open(&f.phone, &profile.identity.device, &result.header, &result).unwrap();
        let receipt: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(receipt["state"], "failed");
        assert!(f.connector.native.calls.borrow().is_empty());
    }
    #[test]
    fn oversized_native_result_has_a_bounded_uncertain_receipt_and_permanent_identity() {
        let f = Fixture::new();
        let engine = f.engine();
        let profile = workspace::load_active(&f.private_dir).unwrap();
        let request = f.request(&profile, REQUEST, &profile.identity.device.device_id, "");
        let digest = request.digest().unwrap();
        let header = serde_json::to_string(&request.header).unwrap();
        f.connector
            .journal
            .encrypted_reserve(REQUEST, &digest, &header, crate::now())
            .unwrap();
        let wire = engine
            .seal_receipt(
                &profile,
                &request.header,
                crate::request::receipt(
                    "completed",
                    json!({"private":"x".repeat(1024*1024)}),
                    None,
                ),
            )
            .unwrap();
        let result: EncryptedEnvelope = serde_json::from_str(&wire).unwrap();
        let plain =
            crypto::open(&f.phone, &profile.identity.device, &result.header, &result).unwrap();
        assert!(plain.len() < 1024);
        let receipt: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(receipt["state"], "uncertain");
        assert_eq!(receipt["request_id"], REQUEST);
        assert!(receipt["result"].is_null());
        assert!(!f
            .connector
            .journal
            .encrypted_reserve(REQUEST, &digest, &header, crate::now())
            .unwrap());
        assert!(f.connector.native.calls.borrow().is_empty());
    }
    #[tokio::test]
    async fn refresh_stops_only_after_a_locally_verified_self_tombstone() {
        let f = Fixture::new();
        let engine = f.engine();
        assert!(engine.refresh().await.is_err());
        assert!(!f.connector.stopping.is_set());
        let mut profile = workspace::load_active(&f.private_dir).unwrap();
        let computer = Identity::from_record(&profile.identity).unwrap();
        let revoke = SignedMembership::sign(
            MembershipStatement {
                workspace_id: profile.membership.workspace_id.clone(),
                revision: 2,
                previous_head: profile.membership.head.clone(),
                issuer_device_id: computer.public().device_id.clone(),
                issuer_key_generation: 1,
                action: MembershipAction::Revoke,
                device: None,
                revoked_device_id: Some(computer.public().device_id.clone()),
            },
            &computer,
        )
        .unwrap();
        profile.membership.apply_next(&revoke).unwrap();
        profile
            .historical_heads
            .insert(2, profile.membership.head.clone());
        let store = Store::lock(&f.private_dir, false).unwrap();
        let revision = Store::revision(store.read("active.json").unwrap().as_deref());
        store
            .replace(
                "active.json",
                &revision,
                &serde_json::to_vec(&profile).unwrap(),
            )
            .unwrap();
        drop(store);
        assert!(engine.refresh().await.is_err());
        assert!(f.connector.stopping.is_set());
    }
    #[tokio::test]
    async fn crash_after_wire_reservation_produces_an_encrypted_uncertain_receipt_without_execution(
    ) {
        let f = Fixture::new();
        let engine = f.engine();
        let profile = workspace::load_active(&f.private_dir).unwrap();
        let envelope = f.request(&profile, REQUEST, &profile.identity.device.device_id, "");
        f.connector
            .journal
            .encrypted_reserve(
                REQUEST,
                &envelope.digest().unwrap(),
                &serde_json::to_string(&envelope.header).unwrap(),
                crate::now(),
            )
            .unwrap();
        assert!(engine.flush(&profile).await.is_err()); // Local port1 refuses POST.
        let pending = f.connector.journal.encrypted_pending().unwrap();
        let saved = pending[0].2.clone().unwrap();
        let result: EncryptedEnvelope = serde_json::from_str(&saved).unwrap();
        let plain =
            crypto::open(&f.phone, &profile.identity.device, &result.header, &result).unwrap();
        let receipt: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(receipt["state"], "uncertain");
        assert!(engine.flush(&profile).await.is_err());
        assert_eq!(
            f.connector.journal.encrypted_pending().unwrap()[0]
                .2
                .as_ref(),
            Some(&saved)
        );
        assert!(f.connector.native.calls.borrow().is_empty());
    }
    include!("encrypted_read_pressure_tests.rs");
}
