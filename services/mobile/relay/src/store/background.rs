use super::*;
use crate::{
    db::{f, Tx},
    protocol::{ATTACHMENT_READ_GRACE, ATTACHMENT_RESULT_TTL, READS, READ_SQL},
};
use std::collections::BTreeSet;
impl Store {
    pub async fn acquire_poll(&self, c: &Credential, ttl: f64) -> Result<String> {
        let mut t = self.db.begin().await?;
        self.db.usage(&mut t, &c.workspace).await?;
        self.authorized_tx(&mut t, c).await?;
        for old in t.all("SELECT id,role,credential_id,workspace_id FROM poll_leases WHERE workspace_id=? AND expires<=? ORDER BY expires LIMIT 256",args![&c.workspace,now()]).await? {Self::release_poll_tx(&mut t,&old).await?;}
        let usage = t
            .one(
                "SELECT active_polls FROM workspace_usage WHERE workspace_id=?",
                args![&c.workspace],
            )
            .await?
            .ok_or(Error::BAD)?;
        t.exec(
            "INSERT INTO poll_credentials(role,credential_id) VALUES(?,?) ON CONFLICT DO NOTHING",
            args![c.role.table(), &c.id],
        )
        .await?;
        let count = t
            .one(
                "SELECT active FROM poll_credentials WHERE role=? AND credential_id=? FOR UPDATE",
                args![c.role.table(), &c.id],
            )
            .await?
            .ok_or(Error::BAD)?;
        if i(&count, "active") >= self.db.cfg.max_polls_per_credential
            || i(&usage, "active_polls") >= self.db.cfg.max_polls_per_workspace
        {
            t.commit().await?;
            return Err(Error::BUSY);
        }
        let lease = Uuid::new_v4().to_string();
        t.exec(
            "INSERT INTO poll_leases(id,role,credential_id,workspace_id,expires) VALUES(?,?,?,?,?)",
            args![&lease, c.role.table(), &c.id, &c.workspace, now() + ttl],
        )
        .await?;
        t.exec(
            "UPDATE poll_credentials SET active=active+1 WHERE role=? AND credential_id=?",
            args![c.role.table(), &c.id],
        )
        .await?;
        t.exec(
            "UPDATE workspace_usage SET active_polls=active_polls+1 WHERE workspace_id=?",
            args![&c.workspace],
        )
        .await?;
        t.commit().await?;
        Ok(lease)
    }
    async fn release_poll_tx(t: &mut Tx, row: &Value) -> Result<()> {
        if t.exec("DELETE FROM poll_leases WHERE id=?", args![&row["id"]])
            .await?
            != 0
        {
            t.exec(
                "UPDATE workspace_usage SET active_polls=active_polls-1 WHERE workspace_id=?",
                args![&row["workspace_id"]],
            )
            .await?;
            t.exec(
                "UPDATE poll_credentials SET active=active-1 WHERE role=? AND credential_id=?",
                args![&row["role"], &row["credential_id"]],
            )
            .await?;
        }
        Ok(())
    }
    pub async fn release_poll_reserved(&self, lease: &str, class: ServiceClass) -> Result<()> {
        if !self.db.cfg.public_enrollment {
            return self.release_poll(lease).await;
        }
        crate::db::retry_cleanup(|| self.release_poll_reserved_once(lease, class)).await
    }
    async fn release_poll_reserved_once(&self, lease: &str, class: ServiceClass) -> Result<()> {
        let mut t = self.db.cleanup_control(class).await?;
        if let Some(row) = t
            .one("SELECT * FROM poll_leases WHERE id=?", args![lease])
            .await?
        {
            self.db.usage(&mut t, s(&row, "workspace_id")).await?;
            Self::release_poll_tx(&mut t, &row).await?;
        }
        t.commit().await
    }
    pub async fn release_poll(&self, lease: &str) -> Result<()> {
        let mut t = self.db.cleanup().await?;
        if let Some(row) = t
            .one("SELECT * FROM poll_leases WHERE id=?", args![lease])
            .await?
        {
            self.db.usage(&mut t, s(&row, "workspace_id")).await?;
            Self::release_poll_tx(&mut t, &row).await?;
        }
        t.commit().await
    }
    pub async fn register_push(&self, c: &Credential, provider: &str, target: &str) -> Result<()> {
        let mut t = self.db.begin().await?;
        self.db.usage(&mut t, &c.workspace).await?;
        self.authorized_tx(&mut t, c).await?;
        t.exec("INSERT INTO pushes(device_id,provider,target) VALUES(?,?,?) ON CONFLICT(device_id) DO UPDATE SET provider=excluded.provider,target=excluded.target",args![&c.id,provider,target]).await?;
        t.commit().await
    }
    pub(super) async fn delete_push_tx(&self, t: &mut Tx, id: &str, w: &str) -> Result<()> {
        let rows = t
            .all("SELECT id FROM push_jobs WHERE device_id=?", args![id])
            .await?;
        for mut r in rows {
            r["workspace_id"] = json!(w);
            self.delete_job(t, &r).await?;
        }
        t.exec("DELETE FROM pushes WHERE device_id=?", args![id])
            .await?;
        Ok(())
    }
    pub async fn delete_push(&self, c: &Credential) -> Result<()> {
        let mut t = self.db.begin().await?;
        t.one("SELECT id FROM global_usage WHERE id=1 FOR UPDATE", &[])
            .await?;
        self.db.usage(&mut t, &c.workspace).await?;
        self.authorized_tx(&mut t, c).await?;
        self.delete_push_tx(&mut t, &c.id, &c.workspace).await?;
        t.commit().await
    }
    pub async fn push_registration(&self, id: &str) -> Result<Option<Value>> {
        self.db.one("SELECT p.provider,p.target FROM pushes p JOIN devices d ON d.id=p.device_id WHERE p.device_id=? AND d.revoked=0",args![id]).await
    }
    pub async fn claim_push_jobs(&self) -> Result<Vec<Value>> {
        Ok(self
            .claim_push_jobs_budgeted()
            .await?
            .into_iter()
            .map(|(job, _budget)| job)
            .collect())
    }
    pub async fn claim_push_jobs_budgeted(&self) -> Result<Vec<(Value, Option<ServiceLease>)>> {
        let mut t = self.db.begin().await?;
        let ordering = if self.db.cfg.public_enrollment {
            "CASE WHEN w.service_class='team' THEN 0 ELSE 1 END,"
        } else {
            ""
        };
        let rows=t.all(&format!("SELECT j.id,j.device_id,j.event_id,j.payload,j.attempts,w.service_class FROM push_jobs j JOIN devices d ON d.id=j.device_id JOIN pushes p ON p.device_id=j.device_id JOIN workspaces w ON w.id=d.workspace_id WHERE j.attempts<5 AND j.next_at<=? AND (j.lease_until IS NULL OR j.lease_until<=?) AND d.revoked=0 ORDER BY {ordering}j.next_at,j.id LIMIT 4 FOR UPDATE SKIP LOCKED"),args![now(),now()]).await?;
        let mut jobs = Vec::new();
        for mut job in rows {
            let class = ServiceClass::parse(s(&job, "service_class"))?;
            let budget = match self.acquire_service("push", class, 1).await {
                Ok(budget) => budget,
                Err(error) if error.0 == Error::BUSY.0 => continue,
                Err(error) => return Err(error),
            };
            job["lease_token"] = json!(token());
            t.exec(
                "UPDATE push_jobs SET lease_token=?,lease_until=? WHERE id=?",
                args![&job["lease_token"], now() + 60.0, &job["id"]],
            )
            .await?;
            jobs.push((job, budget));
        }
        t.commit().await?;
        Ok(jobs)
    }
    pub async fn finish_push(
        &self,
        job: &Value,
        delivered: bool,
        invalid: bool,
        registration: Option<&Value>,
    ) -> Result<()> {
        let mut t = self.db.begin().await?;
        t.one("SELECT id FROM global_usage WHERE id=1 FOR UPDATE", &[])
            .await?;
        let Some(device) = t
            .one(
                "SELECT workspace_id FROM devices WHERE id=?",
                args![&job["device_id"]],
            )
            .await?
        else {
            return Ok(());
        };
        self.db.usage(&mut t, s(&device, "workspace_id")).await?;
        let Some(mut current) = t
            .one(
                "SELECT id,event_id,attempts FROM push_jobs WHERE id=? AND lease_token=? FOR UPDATE",
                args![&job["id"], &job["lease_token"]],
            )
            .await?
        else {
            return Ok(());
        };
        if current["event_id"] != job["event_id"] && !invalid && registration.is_some() {
            // An alert arrived during the send: wake again for it as a new job would.
            t.exec("UPDATE push_jobs SET attempts=0,next_at=?,lease_token=NULL,lease_until=NULL WHERE id=? AND lease_token=?",args![now(),&job["id"],&job["lease_token"]]).await?;
        } else if delivered || invalid || registration.is_none() || i(&current, "attempts") >= 4 {
            current["workspace_id"] = device["workspace_id"].clone();
            self.delete_job(&mut t, &current).await?;
            if invalid {
                if let Some(reg) = registration {
                    t.exec(
                        "DELETE FROM pushes WHERE device_id=? AND provider=? AND target=?",
                        args![&job["device_id"], &reg["provider"], &reg["target"]],
                    )
                    .await?;
                }
            }
        } else {
            t.exec("UPDATE push_jobs SET attempts=attempts+1,next_at=?,lease_token=NULL,lease_until=NULL WHERE id=? AND lease_token=?",args![now()+(5i64*2i64.pow(i(&current,"attempts").clamp(0,10) as u32)).min(3600) as f64,&job["id"],&job["lease_token"]]).await?;
        }
        t.commit().await
    }
    pub(super) async fn elected(t: &mut Tx, key: i64) -> Result<bool> {
        if !t.postgres {
            return Ok(true);
        }
        Ok(t.one(
            "SELECT pg_try_advisory_xact_lock(?) AS acquired",
            args![key],
        )
        .await?
        .is_some_and(|r| r["acquired"] == true))
    }
    async fn payload_locks(&self, t: &mut Tx, rows: &[Value]) -> Result<()> {
        let workspaces: BTreeSet<_> = rows.iter().map(|r| s(r, "workspace_id")).collect();
        let shards: BTreeSet<_> = workspaces.iter().map(|w| self.db.shard(w)).collect();
        for shard in shards {
            t.one(
                "SELECT id FROM payload_shards WHERE id=? FOR UPDATE",
                args![shard],
            )
            .await?;
        }
        for w in workspaces {
            self.db.usage(t, w).await?;
        }
        Ok(())
    }
    pub async fn maintain(&self, restart: bool) -> Result<()> {
        let at = now();
        if self.maintain_records(restart, at).await? {
            // Last, so an alert failure never blocks request expiry or poll cleanup.
            self.maintain_alerts(at).await?;
        }
        Ok(())
    }
    /// Maintenance before serving. Record stages stay fatal; an alert pass
    /// failure is only logged, and the background loop evaluates again.
    pub async fn maintain_at_startup(&self, restart: bool) -> Result<()> {
        let at = now();
        if self.maintain_records(restart, at).await? && self.maintain_alerts(at).await.is_err() {
            eprintln!("relay alert maintenance deferred");
        }
        Ok(())
    }
    /// Request, event, rate and poll maintenance. False when another worker
    /// holds a stage's election, which also skips the alert pass.
    async fn maintain_records(&self, restart: bool, at: f64) -> Result<bool> {
        let batch = self.db.cfg.maintenance_batch;
        {
            let mut t = self.db.begin().await?;
            if !Self::elected(&mut t, 735628110).await? {
                return Ok(false);
            }
            let mut candidates = vec![];
            for (predicate, threshold) in [
                (
                    "state='queued' AND created<=?",
                    at - self.db.cfg.queue_ttl as f64,
                ),
                ("state='queued' AND expires_at<=?", at),
                (
                    "state='claimed' AND claimed<=?",
                    if restart && !self.db.postgres {
                        at
                    } else {
                        at - self.db.cfg.claim_ttl as f64
                    },
                ),
            ] {
                let order = if predicate.contains("claimed<=") {
                    "claimed"
                } else if predicate.contains("expires_at<=") {
                    "expires_at"
                } else {
                    "created"
                };
                candidates.extend(t.all(&format!("SELECT id,workspace_id FROM requests WHERE {predicate} ORDER BY {order},id LIMIT ?"),args![threshold,batch]).await?);
            }
            candidates.sort_by(|a, b| s(a, "id").cmp(s(b, "id")));
            candidates.dedup_by(|a, b| a["id"] == b["id"]);
            candidates.truncate(batch as usize);
            self.payload_locks(&mut t, &candidates).await?;
            for c in candidates {
                if let Some(r)=t.one("SELECT id,workspace_id,state,created,claimed,expires_at,reserved_bytes,result_bytes FROM requests WHERE id=? FOR UPDATE",args![&c["id"]]).await?{self.expire(&mut t,&r,restart&&!self.db.postgres).await?;}
            }
            t.commit().await?;
        }
        {
            let mut t = self.db.begin().await?;
            if !Self::elected(&mut t, 735628111).await? {
                return Ok(false);
            }
            let mut rows = vec![];
            for (predicate, threshold) in [
                ("operation='analytics_read'".into(), at - 120.0),
                ("operation='terminal_snapshot'".into(), at - 120.0),
                // The `task_exchange_history` and `task_done_history` indexes hold only these rows.
                ("operation='task_exchange'".into(), at - 120.0),
                ("operation='task_done'".into(), at - 120.0),
                (
                    format!("operation IN {}", crate::speech_config::READ_SQL),
                    at - 120.0,
                ),
                (
                    format!("operation IN {}", crate::protocol::SPEECH_READ_SQL),
                    at - 120.0,
                ),
                (
                    format!("operation IN {READ_SQL} AND operation!='terminal_snapshot'"),
                    at - self.db.cfg.retention.min(3600) as f64,
                ),
                ("body!=''".into(), at - self.db.cfg.retention as f64),
            ] {
                rows.extend(t.all(&format!("SELECT id,workspace_id FROM requests WHERE {predicate} AND state IN ('completed','failed','uncertain') AND updated<? ORDER BY updated,id LIMIT ?"),args![threshold,batch]).await?);
            }
            // SQLite must not choose the broad state index and sort mutation
            // history for a sparse diagnostic read. PostgreSQL has the same
            // partial date index and uses its normal planner.
            let diagnostics = if self.db.postgres {
                "requests"
            } else {
                "requests INDEXED BY hook_health_history"
            };
            rows.extend(t.all(&format!("SELECT id,workspace_id FROM {diagnostics} WHERE operation='hooks_status' AND state IN ('completed','failed','uncertain') AND updated<? ORDER BY updated,id LIMIT ?"),args![at-120.0,batch]).await?);
            let project_reads = if self.db.postgres {
                "requests"
            } else {
                "requests INDEXED BY project_assignment_history_v1"
            };
            rows.extend(t.all(&format!("SELECT id,workspace_id FROM {project_reads} WHERE operation='project_assignment' AND state IN ('completed','failed','uncertain') AND updated<? ORDER BY updated,id LIMIT ?"),args![at-120.0,batch]).await?);
            // Keep the new read class separate from the persisted read_history_v3 predicate.
            let git_reads = if self.db.postgres {
                "requests"
            } else {
                "requests INDEXED BY git_status_history_v1"
            };
            rows.extend(t.all(&format!("SELECT id,workspace_id FROM {git_reads} WHERE operation='git_status' AND state IN ('completed','failed','uncertain') AND updated<? ORDER BY updated,id LIMIT ?"),args![at-self.db.cfg.retention.min(3600) as f64,batch]).await?);
            let repository_reads = if self.db.postgres {
                "requests"
            } else {
                "requests INDEXED BY repository_reads_v1"
            };
            rows.extend(t.all(&format!("SELECT id,workspace_id FROM {repository_reads} WHERE operation IN {} AND state IN ('completed','failed','uncertain') AND updated<? ORDER BY updated,id LIMIT ?", crate::repositories::READ_SQL),args![at-self.db.cfg.retention.min(3600) as f64,batch]).await?);
            let send_reads = if self.db.postgres {
                "requests"
            } else {
                "requests INDEXED BY send_receipt_reads_v1"
            };
            rows.extend(t.all(&format!("SELECT id,workspace_id FROM {send_reads} WHERE operation='send_receipt' AND state IN ('completed','failed','uncertain') AND updated<? ORDER BY updated,id LIMIT ?"),args![at-self.db.cfg.retention.min(3600) as f64,batch]).await?);
            let projection_reads = if self.db.postgres {
                "requests"
            } else {
                "requests INDEXED BY history_projection_reads_v1"
            };
            rows.extend(t.all(&format!("SELECT id,workspace_id FROM {projection_reads} WHERE operation='history_projection' AND state IN ('completed','failed','uncertain') AND updated<? ORDER BY updated,id LIMIT ?"),args![at-self.db.cfg.retention.min(3600) as f64,batch]).await?);
            rows.extend(t.all("SELECT id,workspace_id FROM requests WHERE operation='archive_list' AND state IN ('completed','failed','uncertain') AND updated<? ORDER BY updated,id LIMIT ?",args![at-3600.0,batch]).await?);
            // Attachment bytes are carried, never kept: a result leaves shortly
            // after the phone read it, or after a short TTL if it never does.
            // The partial `attachment_history` index holds only these rows.
            rows.extend(t.all("SELECT id,workspace_id FROM requests WHERE operation='file_read' AND state IN ('completed','failed','uncertain') AND (updated<? OR result_read<?) ORDER BY updated,id LIMIT ?",args![at-ATTACHMENT_RESULT_TTL,at-ATTACHMENT_READ_GRACE,batch]).await?);
            rows.extend(t.all("SELECT id,workspace_id FROM requests WHERE operation='attachment_read' AND state IN ('completed','failed','uncertain') AND (updated<? OR result_read<?) ORDER BY updated,id LIMIT ?",args![at-ATTACHMENT_RESULT_TTL,at-ATTACHMENT_READ_GRACE,batch]).await?);
            rows.extend(t.all("SELECT id,workspace_id FROM requests WHERE operation IN ('speech_submit','voice_submit','speech_profile_upsert','speech_profile_remove','speech_profile_test') AND body!='' AND state IN ('completed','failed','uncertain') ORDER BY updated,id LIMIT ?",args![batch]).await?);
            rows.sort_by(|a, b| s(a, "id").cmp(s(b, "id")));
            rows.dedup_by(|a, b| a["id"] == b["id"]);
            rows.truncate(batch as usize);
            self.payload_locks(&mut t, &rows).await?;
            for r in rows {
                let Some(r)=t.one("SELECT id,workspace_id,operation,state,updated,result_read,body_bytes,result_bytes,reserved_bytes FROM requests WHERE id=? FOR UPDATE",args![&r["id"]]).await? else{continue;};
                if !["completed", "failed", "uncertain"].contains(&s(&r, "state")) {
                    continue;
                }
                let read = READS.contains(&s(&r, "operation"));
                let attachment = ["attachment_read", "file_read"].contains(&s(&r, "operation"));
                let remove = read
                    && (f(&r, "updated")
                        < at - match s(&r, "operation") {
                            "hooks_status"
                            | "project_assignment"
                            | "analytics_read"
                            | "speech_config"
                            | "speech_config_status"
                            | "terminal_snapshot"
                            | "speech_profiles"
                            | "speech_stats"
                            | "speech_status"
                            | "voice_profiles"
                            | "voice_status"
                            | "task_exchange"
                            | "task_done" => 120.0,
                            "attachment_read" | "file_read" => ATTACHMENT_RESULT_TTL,
                            _ => self.db.cfg.retention.min(3600) as f64,
                        }
                        || (attachment
                            && !r["result_read"].is_null()
                            && f(&r, "result_read") < at - ATTACHMENT_READ_GRACE));
                let size = i(&r, "body_bytes") + i(&r, "result_bytes") + i(&r, "reserved_bytes");
                if remove {
                    t.exec("DELETE FROM requests WHERE id=?", args![&r["id"]])
                        .await?;
                    t.exec(
                        "UPDATE workspace_usage SET reads=reads-1 WHERE workspace_id=?",
                        args![&r["workspace_id"]],
                    )
                    .await?;
                    self.db.bytes(&mut t, s(&r, "workspace_id"), -size).await?;
                } else if [
                    "speech_submit",
                    "voice_submit",
                    "speech_profile_upsert",
                    "speech_profile_remove",
                    "speech_profile_test",
                ]
                .contains(&s(&r, "operation"))
                    && i(&r, "body_bytes") > 0
                {
                    t.exec(
                        "UPDATE requests SET body='',body_bytes=0 WHERE id=?",
                        args![&r["id"]],
                    )
                    .await?;
                    self.db
                        .bytes(&mut t, s(&r, "workspace_id"), -i(&r, "body_bytes"))
                        .await?;
                } else if f(&r, "updated") < at - self.db.cfg.retention as f64
                    && i(&r, "body_bytes") > 0
                {
                    let marker = "request history expired; delivery must not be retried";
                    t.exec("UPDATE requests SET body='',body_bytes=0,result=NULL,result_bytes=?,reserved_bytes=0,error=? WHERE id=?",args![marker.len(),marker,&r["id"]]).await?;
                    self.db
                        .bytes(&mut t, s(&r, "workspace_id"), marker.len() as i64 - size)
                        .await?;
                }
            }
            t.commit().await?;
        }
        {
            let mut t = self.db.begin().await?;
            if !Self::elected(&mut t, 735628112).await? {
                return Ok(false);
            }
            t.one("SELECT id FROM global_usage WHERE id=1 FOR UPDATE", &[])
                .await?;
            let events=t.all("SELECT id,workspace_id FROM events WHERE created<? ORDER BY created,id LIMIT ?",args![at-self.db.cfg.retention as f64,batch]).await?;
            let jobs=t.all("SELECT j.id,d.workspace_id FROM push_jobs j JOIN devices d ON d.id=j.device_id WHERE j.created<? ORDER BY j.created,j.id LIMIT ?",args![at-86400.0,batch]).await?;
            let spaces: BTreeSet<_> = events
                .iter()
                .chain(jobs.iter())
                .map(|r| s(r, "workspace_id"))
                .collect();
            for w in spaces {
                self.db.usage(&mut t, w).await?;
            }
            for r in events {
                self.delete_event(&mut t, &r).await?;
            }
            for r in jobs {
                self.delete_job(&mut t, &r).await?;
            }
            t.exec("DELETE FROM invitations WHERE code_hash IN (SELECT code_hash FROM invitations WHERE expires<=? ORDER BY expires LIMIT ?)",args![at,batch]).await?;
            t.commit().await?;
        }
        {
            let mut t = self.db.begin().await?;
            if !Self::elected(&mut t, 735628113).await? {
                return Ok(false);
            }
            t.all("SELECT id FROM rate_control ORDER BY id FOR UPDATE", &[])
                .await?;
            // RETURNING counts only rows actually deleted, including renewals racing cleanup.
            let rates=t.all("DELETE FROM rate_limits WHERE key IN (SELECT key FROM rate_limits WHERE expires<=? ORDER BY expires LIMIT ?) AND expires<=? RETURNING bucket",args![at,batch,at]).await?;
            for bucket in [1i64, 2] {
                let n = rates.iter().filter(|r| i(r, "bucket") == bucket).count();
                t.exec(
                    "UPDATE rate_control SET entries=entries-? WHERE id=?",
                    args![n, bucket],
                )
                .await?;
            }
            t.commit().await?;
        }
        let mut t = self.db.cleanup().await?;
        let leases = t
            .all(
                "SELECT id FROM poll_leases WHERE expires<=? ORDER BY expires,id LIMIT ?",
                args![at, batch],
            )
            .await?;
        t.commit().await?;
        for l in leases {
            self.release_poll(s(&l, "id")).await?;
        }
        Ok(true)
    }
}
