CREATE TABLE IF NOT EXISTS team_member_readability (
 team TEXT NOT NULL,
 member TEXT NOT NULL,
 marked INTEGER NOT NULL CHECK(marked IN (0,1)),
 pending_since_ms INTEGER CHECK(pending_since_ms >= 0),
 PRIMARY KEY(team,member),
 FOREIGN KEY(team,member) REFERENCES members(team,id) ON DELETE CASCADE,
 CHECK(marked OR pending_since_ms IS NOT NULL)
);
CREATE TRIGGER IF NOT EXISTS team_member_readability_binding_v1
AFTER UPDATE OF machine,session_id,state ON members
WHEN OLD.machine IS NOT NEW.machine OR OLD.session_id IS NOT NEW.session_id
 OR OLD.state IS NOT NEW.state
BEGIN
 DELETE FROM team_member_readability WHERE team=OLD.team AND member=OLD.id;
END;
