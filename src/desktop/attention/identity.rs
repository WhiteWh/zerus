//! Stable effect identities contain only the original notification target.
//! Source epochs, host leases, receipt IDs and presentation never enter the key.
use sha2::{Digest, Sha256};
use zerus_desktop_ledger::{Identity, Kind, Target};

pub(super) enum Edge<'a> {
    Reply(&'a str),
    Attention(&'a str),
}

pub(super) struct Key {
    pub id: String,
    pub identity: Identity,
}

fn valid(value: &str, cap: usize) -> bool {
    !value.is_empty() && value.len() <= cap && !value.chars().any(char::is_control)
}

impl Key {
    pub fn new(computer: &str, run: &str, conversation: &str, edge: Edge<'_>) -> Option<Self> {
        if (!computer.is_empty() && !valid(computer, 255))
            || !valid(run, 1024)
            || !valid(conversation, 1024)
        {
            return None;
        }
        let edge = match edge {
            Edge::Reply(reply) if valid(reply, 1000) => format!("reply:{reply}"),
            Edge::Attention(signature)
                if signature.len() == 64
                    && signature
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) =>
            {
                format!("attention:{signature}")
            }
            _ => return None,
        };
        let identity = Identity {
            kind: Kind::Post,
            target: Target::Session {
                computer: computer.into(),
                run: run.into(),
                conversation: conversation.into(),
            },
            edge,
        };
        // Exactly the ledger's canonical identity encoding; payload and host
        // session are deliberately absent. put_intent still validates it.
        let encoded = serde_json::to_vec(&identity).ok()?;
        Some(Self {
            id: format!("{:x}", Sha256::digest(encoded)),
            identity,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_and_ledger_use_the_same_stable_target_key() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        let tx = db.transaction().unwrap();
        zerus_desktop_ledger::migrate(&tx).unwrap();
        tx.commit().unwrap();
        let key = Key::new("", "run", "conversation", Edge::Reply("reply-1")).unwrap();
        let stored = zerus_desktop_ledger::put_intent(
            &mut db,
            &key.identity,
            1,
            &serde_json::json!({"presentation":"the first observed caption"}),
        )
        .unwrap();
        assert_eq!(key.id, stored);
        assert_eq!(
            Key::new("", "run", "conversation", Edge::Reply("reply-1"))
                .unwrap()
                .id,
            stored,
            "reconnect/restart/upgrade leases are not part of the key"
        );
        for different in [
            Key::new(
                "peer.example",
                "run",
                "conversation",
                Edge::Reply("reply-1"),
            ),
            Key::new("", "other-run", "conversation", Edge::Reply("reply-1")),
            Key::new("", "run", "other-conversation", Edge::Reply("reply-1")),
            Key::new("", "run", "conversation", Edge::Reply("reply-2")),
            Key::new("", "run", "conversation", Edge::Attention(&"a".repeat(64))),
        ] {
            assert_ne!(different.unwrap().id, stored);
        }
    }

    #[test]
    fn missing_or_ambiguous_native_identity_has_no_name_or_epoch_fallback() {
        for (run, conversation, reply) in [("", "c", "r"), ("r", "", "r"), ("r", "c", "")] {
            assert!(Key::new("", run, conversation, Edge::Reply(reply)).is_none());
        }
        for signature in ["", "unknown", &"A".repeat(64), &"a".repeat(65)] {
            assert!(Key::new("", "run", "c", Edge::Attention(signature)).is_none());
        }
        assert!(Key::new("", "run\nother", "c", Edge::Reply("r")).is_none());
    }
}
