-- Same durable model in exclusive local mode.

CREATE TABLE IF NOT EXISTS relay_schema(version integer PRIMARY KEY);
INSERT INTO relay_schema VALUES(1) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS workspaces(id text PRIMARY KEY,name text NOT NULL,service_class text NOT NULL DEFAULT 'standard');
CREATE TABLE IF NOT EXISTS nodes(id text PRIMARY KEY,workspace_id text NOT NULL REFERENCES workspaces(id),name text NOT NULL,token_hash text UNIQUE NOT NULL,revoked integer NOT NULL DEFAULT 0,last_seen double precision,snapshot text,snapshot_hash text);
CREATE INDEX IF NOT EXISTS node_workspace ON nodes(workspace_id,name,id) WHERE revoked=0;
CREATE TABLE IF NOT EXISTS devices(id text PRIMARY KEY,workspace_id text NOT NULL REFERENCES workspaces(id),name text NOT NULL,token_hash text UNIQUE NOT NULL,revoked integer NOT NULL DEFAULT 0,paired_at double precision,last_seen double precision,added_by text);
CREATE INDEX IF NOT EXISTS device_workspace ON devices(workspace_id) WHERE revoked=0;
CREATE TABLE IF NOT EXISTS invitations(code_hash text PRIMARY KEY,workspace_id text NOT NULL REFERENCES workspaces(id),expires double precision NOT NULL,added_by text);
CREATE INDEX IF NOT EXISTS invitation_expiry ON invitations(expires);
CREATE TABLE IF NOT EXISTS requests(id text PRIMARY KEY,workspace_id text NOT NULL REFERENCES workspaces(id),device_id text NOT NULL REFERENCES devices(id),node_id text NOT NULL REFERENCES nodes(id),operation text NOT NULL,body_hash text NOT NULL,body_bytes INTEGER NOT NULL,body text NOT NULL,state text NOT NULL,result text,error text,created double precision NOT NULL,claimed double precision,updated double precision NOT NULL,expires_at double precision,result_read double precision,result_bytes INTEGER NOT NULL DEFAULT 0,reserved_bytes INTEGER NOT NULL DEFAULT 0);
DROP INDEX IF EXISTS read_history_v2;
CREATE INDEX IF NOT EXISTS request_queue ON requests(node_id,created,id) WHERE state='queued';
CREATE INDEX IF NOT EXISTS request_queue_expiry ON requests(created,id) WHERE state='queued';
CREATE INDEX IF NOT EXISTS request_claim_expiry ON requests(claimed,id) WHERE state='claimed';
CREATE INDEX IF NOT EXISTS terminal_expiry ON requests(expires_at,id) WHERE state='queued' AND expires_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS terminal_history ON requests(updated,id) WHERE operation='terminal_snapshot' AND state IN ('completed','failed','uncertain');
CREATE INDEX IF NOT EXISTS attachment_history ON requests(updated,id) WHERE operation='attachment_read' AND state IN ('completed','failed','uncertain');
CREATE INDEX IF NOT EXISTS request_history ON requests(updated,id) WHERE body!='' AND state IN ('completed','failed','uncertain');
CREATE INDEX IF NOT EXISTS read_history_v3 ON requests(updated,id) WHERE operation IN ('inspect','history','process_output','terminal_snapshot','catalog','dirs','worktrees','mark_read') AND state IN ('completed','failed','uncertain');
CREATE INDEX IF NOT EXISTS request_workspace ON requests(workspace_id,state);
CREATE TABLE IF NOT EXISTS workspace_usage(workspace_id text PRIMARY KEY REFERENCES workspaces(id),payload_bytes INTEGER NOT NULL DEFAULT 0,active INTEGER NOT NULL DEFAULT 0,reads INTEGER NOT NULL DEFAULT 0,mutations INTEGER NOT NULL DEFAULT 0,events INTEGER NOT NULL DEFAULT 0,push_jobs INTEGER NOT NULL DEFAULT 0,active_polls INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS payload_shards(id integer PRIMARY KEY,payload_bytes INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS relay_allocation(id integer PRIMARY KEY CHECK(id=1),global_max_bytes INTEGER NOT NULL,quota_shards integer NOT NULL);
CREATE TABLE IF NOT EXISTS rate_control(id integer PRIMARY KEY,entries INTEGER NOT NULL DEFAULT 0);
INSERT INTO rate_control(id) VALUES(1),(2) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS global_usage(id integer PRIMARY KEY CHECK(id=1),payload_bytes INTEGER NOT NULL DEFAULT 0,events INTEGER NOT NULL DEFAULT 0,push_jobs INTEGER NOT NULL DEFAULT 0);
INSERT INTO global_usage(id) VALUES(1) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY AUTOINCREMENT,workspace_id text NOT NULL REFERENCES workspaces(id),node_id text NOT NULL,session text NOT NULL,kind text NOT NULL,created double precision NOT NULL,alert_kind text);
CREATE INDEX IF NOT EXISTS event_workspace ON events(workspace_id,id);
CREATE INDEX IF NOT EXISTS event_expiry ON events(created,id);
CREATE TABLE IF NOT EXISTS pushes(device_id text PRIMARY KEY REFERENCES devices(id),provider text NOT NULL,target text NOT NULL);
CREATE TABLE IF NOT EXISTS push_jobs(id INTEGER PRIMARY KEY AUTOINCREMENT,device_id text NOT NULL REFERENCES devices(id),event_id INTEGER NOT NULL,payload text NOT NULL,attempts integer NOT NULL DEFAULT 0,next_at double precision NOT NULL,created double precision NOT NULL,lease_token text,lease_until double precision);
CREATE INDEX IF NOT EXISTS push_device ON push_jobs(device_id,id);
CREATE INDEX IF NOT EXISTS push_ready ON push_jobs(next_at,id) WHERE attempts<5;
CREATE INDEX IF NOT EXISTS push_expiry ON push_jobs(created,id);
CREATE TABLE IF NOT EXISTS poll_credentials(role text NOT NULL,credential_id text NOT NULL,active INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(role,credential_id));
CREATE TABLE IF NOT EXISTS poll_leases(id text PRIMARY KEY,role text NOT NULL,credential_id text NOT NULL,workspace_id text NOT NULL REFERENCES workspaces(id),expires double precision NOT NULL);
CREATE INDEX IF NOT EXISTS poll_expiry ON poll_leases(expires,id);
CREATE TABLE IF NOT EXISTS rate_limits(key text PRIMARY KEY,count INTEGER NOT NULL,expires double precision NOT NULL,bucket integer NOT NULL DEFAULT 1);
CREATE INDEX IF NOT EXISTS rate_expiry ON rate_limits(expires,key);


CREATE TABLE IF NOT EXISTS computers(id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL REFERENCES workspaces(id),name TEXT NOT NULL,machine_id TEXT,revoked INTEGER NOT NULL DEFAULT 0,exposed INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS computer_workspace ON computers(workspace_id,id);
CREATE TABLE IF NOT EXISTS native_computers(workspace_id TEXT NOT NULL REFERENCES workspaces(id),machine_id TEXT NOT NULL,computer_id TEXT NOT NULL REFERENCES computers(id),PRIMARY KEY(workspace_id,machine_id));
CREATE TABLE IF NOT EXISTS computer_aliases(workspace_id TEXT NOT NULL REFERENCES workspaces(id),alias TEXT NOT NULL,computer_id TEXT NOT NULL REFERENCES computers(id),PRIMARY KEY(workspace_id,alias));
CREATE INDEX IF NOT EXISTS computer_alias_target ON computer_aliases(computer_id);
CREATE TABLE IF NOT EXISTS computer_routes(gateway_id TEXT NOT NULL REFERENCES nodes(id),route_id TEXT NOT NULL,computer_id TEXT NOT NULL REFERENCES computers(id),machine_id TEXT,local INTEGER NOT NULL DEFAULT 0,active INTEGER NOT NULL DEFAULT 1,online INTEGER NOT NULL DEFAULT 0,guarded INTEGER NOT NULL DEFAULT 0,name TEXT NOT NULL,snapshot TEXT,snapshot_hash TEXT,last_seen DOUBLE PRECISION,presence_at DOUBLE PRECISION,presence_until DOUBLE PRECISION,PRIMARY KEY(gateway_id,route_id));
CREATE INDEX IF NOT EXISTS computer_route_target ON computer_routes(computer_id,gateway_id,route_id);
INSERT INTO computers(id,workspace_id,name,revoked,exposed) SELECT id,workspace_id,name,revoked,1 FROM nodes WHERE computer_id IS NULL ON CONFLICT(id) DO NOTHING;
INSERT INTO computer_aliases(workspace_id,alias,computer_id) SELECT workspace_id,id,id FROM nodes WHERE computer_id IS NULL ON CONFLICT(workspace_id,alias) DO NOTHING;
INSERT INTO computer_routes(gateway_id,route_id,computer_id,local,active,online,name) SELECT id,id,id,1,1,1,name FROM nodes WHERE computer_id IS NULL ON CONFLICT(gateway_id,route_id) DO NOTHING;
UPDATE nodes SET computer_id=id WHERE computer_id IS NULL;

-- Presence-aware phone alerts (additive; relay_schema stays 1).
CREATE TABLE IF NOT EXISTS alert_candidates(id INTEGER PRIMARY KEY AUTOINCREMENT,workspace_id TEXT NOT NULL REFERENCES workspaces(id),computer_id TEXT NOT NULL,session TEXT NOT NULL,kind TEXT NOT NULL,run_id TEXT,conversation_id TEXT,attention_signature TEXT,reply_id TEXT,created DOUBLE PRECISION NOT NULL,next_at DOUBLE PRECISION NOT NULL,UNIQUE(workspace_id,computer_id,session,kind));
CREATE INDEX IF NOT EXISTS alert_candidate_due ON alert_candidates(next_at,id);
CREATE INDEX IF NOT EXISTS alert_candidate_expiry ON alert_candidates(created,id);

CREATE INDEX IF NOT EXISTS speech_reads_history ON requests(updated,id) WHERE operation IN ('speech_profiles','speech_stats','speech_status') AND state IN ('completed','failed','uncertain');
CREATE INDEX IF NOT EXISTS speech_audio_cleanup ON requests(updated,id) WHERE operation='speech_submit' AND body!='' AND state IN ('completed','failed','uncertain');

CREATE INDEX IF NOT EXISTS speech_config_reads_history ON requests(updated,id) WHERE operation IN ('speech_config','speech_config_status') AND state IN ('completed','failed','uncertain');
CREATE INDEX IF NOT EXISTS speech_config_body_cleanup ON requests(updated,id) WHERE operation IN ('speech_profile_upsert','speech_profile_remove','speech_profile_test') AND body!='' AND state IN ('completed','failed','uncertain');

CREATE INDEX IF NOT EXISTS file_history ON requests(updated,id) WHERE operation='file_read' AND state IN ('completed','failed','uncertain');

CREATE INDEX IF NOT EXISTS archive_history ON requests(updated,id) WHERE operation='archive_list' AND state IN ('completed','failed','uncertain');

CREATE INDEX IF NOT EXISTS task_exchange_history ON requests(updated,id) WHERE operation='task_exchange' AND state IN ('completed','failed','uncertain');

CREATE INDEX IF NOT EXISTS task_done_history ON requests(updated,id) WHERE operation='task_done' AND state IN ('completed','failed','uncertain');
-- Content-free hook diagnostics expire independently of native mutations.
CREATE INDEX IF NOT EXISTS hook_health_history ON requests(updated,id) WHERE operation='hooks_status' AND state IN ('completed','failed','uncertain');

CREATE INDEX IF NOT EXISTS analytics_reads_history ON requests(updated,id) WHERE operation='analytics_read' AND state IN ('completed','failed','uncertain');

CREATE INDEX IF NOT EXISTS git_status_history_v1 ON requests(updated,id) WHERE operation='git_status' AND state IN ('completed','failed','uncertain');

-- Permanent shared-notification highwater, independent of gateway route lifetime.
CREATE TABLE IF NOT EXISTS computer_notification_preferences(computer_id TEXT PRIMARY KEY REFERENCES computers(id),workspace_id TEXT NOT NULL REFERENCES workspaces(id),preferences TEXT NOT NULL,payload_bytes BIGINT NOT NULL);
CREATE INDEX IF NOT EXISTS computer_notification_workspace ON computer_notification_preferences(workspace_id,computer_id);

-- Projection reads have their own class; the persisted read_history_v3 stays unchanged.
CREATE INDEX IF NOT EXISTS history_projection_reads_v1 ON requests(updated,id) WHERE operation='history_projection' AND state IN ('completed','failed','uncertain');
-- Additive index for session-project-v1 reads; legacy read_history_v3 and
-- mutation UUID tombstones are deliberately unchanged on upgrade.
CREATE INDEX IF NOT EXISTS project_assignment_history_v1 ON requests(updated,id) WHERE operation='project_assignment' AND state IN ('completed','failed','uncertain');

CREATE INDEX IF NOT EXISTS repository_reads_v1 ON requests(updated,id) WHERE operation IN ('repository_check','repository_status') AND state IN ('completed','failed','uncertain');

-- Public service admission: configuration is pinned across all workers.
CREATE TABLE IF NOT EXISTS service_resources(lane TEXT PRIMARY KEY,total BIGINT NOT NULL,standard BIGINT NOT NULL,used_total BIGINT NOT NULL DEFAULT 0,used_standard BIGINT NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS service_leases(id TEXT PRIMARY KEY,lane TEXT NOT NULL REFERENCES service_resources(lane),service_class TEXT NOT NULL,units BIGINT NOT NULL,expires DOUBLE PRECISION NOT NULL);
CREATE INDEX IF NOT EXISTS service_lease_expiry ON service_leases(lane,expires,id);

-- Encrypted service records contain only public membership and routing data.
CREATE TABLE IF NOT EXISTS encrypted_workspaces(workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id),membership_head TEXT NOT NULL,membership_revision BIGINT NOT NULL);
CREATE TABLE IF NOT EXISTS encrypted_members(workspace_id TEXT NOT NULL REFERENCES workspaces(id),device_id TEXT NOT NULL,role TEXT NOT NULL,public_record TEXT NOT NULL,revoked INTEGER NOT NULL DEFAULT 0,revoked_revision BIGINT,PRIMARY KEY(workspace_id,device_id));
CREATE INDEX IF NOT EXISTS encrypted_member_device ON encrypted_members(device_id,workspace_id);
CREATE TABLE IF NOT EXISTS membership_changes(workspace_id TEXT NOT NULL REFERENCES workspaces(id),revision BIGINT NOT NULL,change_id TEXT NOT NULL,head TEXT NOT NULL,record TEXT NOT NULL,PRIMARY KEY(workspace_id,revision),UNIQUE(workspace_id,change_id));
CREATE TABLE IF NOT EXISTS enrollment_invitations(invitation_id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL REFERENCES workspaces(id),issuer_device_id TEXT NOT NULL,purpose TEXT NOT NULL,expires DOUBLE PRECISION NOT NULL,used INTEGER NOT NULL DEFAULT 0,membership_head TEXT NOT NULL,membership_revision BIGINT NOT NULL,bootstrap TEXT NOT NULL,metadata_bytes BIGINT NOT NULL);
CREATE INDEX IF NOT EXISTS enrollment_invitation_workspace ON enrollment_invitations(workspace_id,expires,invitation_id);
CREATE TABLE IF NOT EXISTS enrollment_candidates(device_id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL REFERENCES workspaces(id),invitation_id TEXT NOT NULL REFERENCES enrollment_invitations(invitation_id),candidate TEXT NOT NULL,candidate_hash TEXT NOT NULL,token_hash TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',created DOUBLE PRECISION NOT NULL,approval TEXT,metadata_bytes BIGINT NOT NULL);
CREATE INDEX IF NOT EXISTS enrollment_pending ON enrollment_candidates(workspace_id,state,device_id);
CREATE INDEX IF NOT EXISTS enrollment_invitation_candidates ON enrollment_candidates(invitation_id,state,device_id);
CREATE TABLE IF NOT EXISTS public_registration(id INTEGER PRIMARY KEY,workspaces BIGINT NOT NULL DEFAULT 0);
INSERT INTO public_registration(id) VALUES(1) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS encrypted_requests(id TEXT PRIMARY KEY,workspace_id TEXT NOT NULL REFERENCES workspaces(id),device_id TEXT NOT NULL REFERENCES devices(id),node_id TEXT NOT NULL REFERENCES nodes(id),route_id TEXT NOT NULL,body_hash TEXT NOT NULL,body TEXT NOT NULL,body_bytes BIGINT NOT NULL,state TEXT NOT NULL,result TEXT,result_hash TEXT,result_bytes BIGINT NOT NULL DEFAULT 0,reserved_bytes BIGINT NOT NULL DEFAULT 0,created DOUBLE PRECISION NOT NULL,claimed DOUBLE PRECISION,updated DOUBLE PRECISION NOT NULL,expires DOUBLE PRECISION NOT NULL,metadata_bytes BIGINT NOT NULL DEFAULT 0,payload_class BIGINT NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS encrypted_request_queue ON encrypted_requests(node_id,state,created,id);
CREATE INDEX IF NOT EXISTS encrypted_request_expiry ON encrypted_requests(expires,id) WHERE body_bytes>0 OR result_bytes>0 OR reserved_bytes>0;
CREATE INDEX IF NOT EXISTS encrypted_request_active ON encrypted_requests(workspace_id,state);
CREATE INDEX IF NOT EXISTS encrypted_request_device ON encrypted_requests(workspace_id,device_id,state);
CREATE TABLE IF NOT EXISTS encrypted_route_bindings(node_id TEXT NOT NULL REFERENCES nodes(id),route_id TEXT NOT NULL,computer_id TEXT NOT NULL,workspace_id TEXT NOT NULL REFERENCES workspaces(id),metadata_bytes BIGINT NOT NULL DEFAULT 0,PRIMARY KEY(node_id,route_id));
CREATE TABLE IF NOT EXISTS encrypted_routes(node_id TEXT NOT NULL REFERENCES nodes(id),route_id TEXT NOT NULL,computer_id TEXT NOT NULL,workspace_id TEXT NOT NULL REFERENCES workspaces(id),last_seen DOUBLE PRECISION,presence_at DOUBLE PRECISION,presence_until DOUBLE PRECISION,PRIMARY KEY(node_id,route_id));
CREATE INDEX IF NOT EXISTS encrypted_route_catalog ON encrypted_routes(workspace_id,computer_id,node_id,route_id);
CREATE TABLE IF NOT EXISTS encrypted_snapshots(node_id TEXT NOT NULL REFERENCES nodes(id),route_id TEXT NOT NULL,device_id TEXT NOT NULL REFERENCES devices(id),workspace_id TEXT NOT NULL REFERENCES workspaces(id),body TEXT NOT NULL,body_bytes BIGINT NOT NULL,updated DOUBLE PRECISION NOT NULL,PRIMARY KEY(node_id,route_id,device_id));
CREATE INDEX IF NOT EXISTS encrypted_snapshot_workspace ON encrypted_snapshots(workspace_id,node_id,route_id,device_id);

CREATE TABLE IF NOT EXISTS opaque_signals(workspace_id TEXT NOT NULL REFERENCES workspaces(id),node_id TEXT NOT NULL REFERENCES nodes(id),scope_id TEXT NOT NULL,kind TEXT NOT NULL,route_id TEXT NOT NULL,computer_id TEXT NOT NULL,change_id TEXT NOT NULL,active INTEGER NOT NULL,delivered INTEGER NOT NULL DEFAULT 0,created DOUBLE PRECISION NOT NULL,next_at DOUBLE PRECISION NOT NULL,metadata_bytes BIGINT NOT NULL DEFAULT 0,PRIMARY KEY(workspace_id,node_id,scope_id,kind));
CREATE INDEX IF NOT EXISTS opaque_signal_due ON opaque_signals(next_at,workspace_id,node_id,scope_id,kind) WHERE active=1 AND delivered=0;

CREATE INDEX IF NOT EXISTS workspace_service_class ON workspaces(service_class,id);
CREATE INDEX IF NOT EXISTS node_service_auth ON nodes(workspace_id,token_hash) WHERE revoked=0;
CREATE INDEX IF NOT EXISTS device_service_auth ON devices(workspace_id,token_hash) WHERE revoked=0;

CREATE INDEX IF NOT EXISTS enrollment_invitation_expiry ON enrollment_invitations(expires,invitation_id) WHERE used=0;

CREATE TABLE IF NOT EXISTS encrypted_request_chunks(request_id TEXT NOT NULL REFERENCES encrypted_requests(id),sequence BIGINT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(request_id,sequence));

CREATE INDEX IF NOT EXISTS encrypted_request_unaccounted ON encrypted_requests(workspace_id,id) WHERE metadata_bytes=0;
CREATE INDEX IF NOT EXISTS encrypted_binding_unaccounted ON encrypted_route_bindings(workspace_id,node_id,route_id) WHERE metadata_bytes=0;
CREATE INDEX IF NOT EXISTS opaque_signal_unaccounted ON opaque_signals(workspace_id,node_id,scope_id,kind) WHERE metadata_bytes=0;

CREATE INDEX IF NOT EXISTS encrypted_payload_classification ON encrypted_requests(id) WHERE state='completed' AND payload_class IN (0,3) AND result_bytes>0;
CREATE INDEX IF NOT EXISTS encrypted_read_cache_pressure ON encrypted_requests(workspace_id,updated,id) WHERE state='completed' AND payload_class=2 AND reserved_bytes=0 AND (body_bytes>0 OR result_bytes>0);

-- Exact original-send recovery is a separate bounded READ class.
CREATE INDEX IF NOT EXISTS send_receipt_reads_v1 ON requests(updated,id) WHERE operation='send_receipt' AND state IN ('completed','failed','uncertain');
