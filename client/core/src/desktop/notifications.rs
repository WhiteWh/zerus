//! Service/native-host wire data. Only the service actor reserves effects;
//! decoding these values grants no dispatch authority or notification ownership.
use crate::{attention, desktop::digest};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, Write};
use zerus_desktop_migration::canonical_uuid;

pub mod macos;

pub const ABI: &str = "desktop-notification-host-v1";
pub const ENTRY: &str = "--desktop-notification-host";
pub const APPLICATION: &str = "com.hgdev.hgs-tray";
/// Set in the sealed app plist only when its fixed host entry is implemented.
pub const CAPABILITY_KEY: &str = "HGSDesktopNotificationHostABI";
pub const FRAME_BYTES: usize = 64 << 10;
pub const IN_FLIGHT: usize = 8;
pub const MAPPINGS: usize = 8192;
pub const TOKEN_BYTES: usize = 8192;
pub const ACTIVATION_TOKEN_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    Macos,
    Windows,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub configuration: String,
    pub service_epoch: String,
    pub writer_epoch: String,
    pub adapter: String,
    pub host_epoch: String,
    pub lease: String,
    pub platform: Platform,
}
impl Session {
    pub fn valid(&self) -> bool {
        digest(&self.configuration)
            && [
                &self.service_epoch,
                &self.writer_epoch,
                &self.adapter,
                &self.host_epoch,
                &self.lease,
            ]
            .iter()
            .all(|v| canonical_uuid(v))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub session: Session,
    pub request_id: String,
    pub intent_id: String,
    pub revision: u64,
    pub seq: i64,
}
impl Binding {
    pub fn valid(&self) -> bool {
        self.session.valid()
            && canonical_uuid(&self.request_id)
            && digest(&self.intent_id)
            && self.seq > 0
            && (1..=i64::MAX as u64).contains(&self.revision)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeId {
    Linux {
        bus_id: String,
        owner: String,
        id: u32,
    },
    Macos {
        request_id: String,
    },
    Windows {
        tag: String,
        group: String,
    },
}
impl NativeId {
    pub fn valid_for(&self, platform: Platform, token: &str) -> bool {
        match (self, platform) {
            (Self::Linux { bus_id, owner, id }, Platform::Linux) => {
                *id != 0
                    && bus_id.len() == 32
                    && bus_id.bytes().all(|b| b.is_ascii_hexdigit())
                    && owner.len() <= 255
                    && owner.starts_with(':')
                    && owner.len() > 1
                    && owner[1..]
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
            }
            (Self::Macos { request_id }, Platform::Macos) => {
                *request_id == attention::request_id(token)
            }
            (Self::Windows { tag, group }, Platform::Windows) => {
                bounded(tag, 64) && bounded(group, 64)
            }
            _ => false,
        }
    }
}

fn bounded(text: &str, cap: usize) -> bool {
    !text.is_empty() && text.len() <= cap && !text.chars().any(char::is_control)
}
fn valid_token(token: &str) -> bool {
    token.len() <= TOKEN_BYTES && attention::target(token).is_some()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Post {
        token: String,
        title: String,
        body: String,
    },
    Withdraw {
        token: String,
        native_id: NativeId,
    },
}
impl Action {
    pub fn token(&self) -> &str {
        match self {
            Self::Post { token, .. } | Self::Withdraw { token, .. } => token,
        }
    }
    pub fn valid(&self, platform: Platform) -> bool {
        valid_token(self.token())
            && match self {
                Self::Post { title, body, .. } => {
                    bounded(title, 512)
                        && body.len() <= 4096
                        && !body
                            .chars()
                            .any(|c| c.is_control() && c != '\n' && c != '\t')
                }
                Self::Withdraw { token, native_id } => native_id.valid_for(platform, token),
            }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub binding: Binding,
    pub action: Action,
}
impl Command {
    pub fn valid(&self) -> bool {
        self.binding.valid() && self.action.valid(self.binding.session.platform)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    NotReady,
    Cancelled,
    InvalidCommand,
    Unsupported,
    Capacity,
    PermissionDenied,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Uncertainty {
    Transport,
    Deadline,
    NativeError,
    HostLost,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Delivered {
        native_id: NativeId,
    },
    /// Only a known refusal BEFORE the native posting/withdrawal API.
    Refused {
        code: Refusal,
    },
    Unknown {
        code: Uncertainty,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub binding: Binding,
    pub outcome: Outcome,
}
impl Receipt {
    pub fn matches(&self, command: &Command) -> bool {
        self.binding == command.binding
            && match &self.outcome {
                Outcome::Delivered { native_id } => {
                    native_id.valid_for(command.binding.session.platform, command.action.token())
                        && match &command.action {
                            Action::Withdraw {
                                native_id: expected,
                                ..
                            } => native_id == expected,
                            _ => true,
                        }
                }
                _ => true,
            }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "platform", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlatformContext {
    Wayland { activation_token: String },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    /// Original post request UUID, preserved as native callback correlation.
    /// The host never rebuilds a service binding or keeps a second ledger.
    pub request_id: String,
    pub token: String,
    pub native_id: NativeId,
    /// Ephemeral UI-open context, never notification identity or restoration
    /// data. The adapter consumes the token paired with this native callback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_context: Option<PlatformContext>,
}
impl Activation {
    pub fn valid_for(&self, platform: Platform) -> bool {
        canonical_uuid(&self.request_id)
            && self.token.len() <= TOKEN_BYTES
            && attention::target(&self.token).is_some()
            && self.native_id.valid_for(platform, &self.token)
            && self
                .platform_context
                .as_ref()
                .is_none_or(|context| match context {
                    PlatformContext::Wayland { activation_token } => {
                        platform == Platform::Linux
                            && !activation_token.is_empty()
                            && activation_token.len() <= ACTIVATION_TOKEN_BYTES
                            && !activation_token.chars().any(char::is_control)
                    }
                })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToHost {
    Hello {
        abi: String,
        session: Session,
    },
    Command(Command),
    /// Rebuild a bounded callback ID cache only. Never call a posting or close
    /// API for this message; unknown/reserved attempts are never restored.
    Restore {
        session: Session,
        mapping: Activation,
    },
    Stop {
        session: Session,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
pub enum FromHost {
    Ready {
        abi: String,
        application: String,
        session: Session,
    },
    Receipt(Receipt),
    /// The original receipt output and all command-local native captures have
    /// actually drained. This is resource release, not a new effect result.
    Released {
        binding: Binding,
    },
    Clicked(Activation),
    Closed(Activation),
    /// Sent only after every original command/callback and resource is drained.
    Stopped {
        session: Session,
    },
}

/// One bounded NDJSON line, including LF. Callers must also impose their
/// original nonblocking transport deadline, preserve partial input and refuse
/// EOF/overflow. This encoder never invents success on an over-limit result.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, &'static str> {
    struct Frame {
        bytes: Vec<u8>,
        overflow: bool,
    }
    impl Write for Frame {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > (FRAME_BYTES - 1).saturating_sub(self.bytes.len()) {
                self.overflow = true;
                return Err(io::ErrorKind::InvalidData.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut frame = Frame {
        bytes: Vec::new(),
        overflow: false,
    };
    if serde_json::to_writer(&mut frame, value).is_err() {
        return Err(if frame.overflow {
            "notification_frame_limit"
        } else {
            "notification_frame_invalid"
        });
    }
    frame.bytes.push(b'\n');
    Ok(frame.bytes)
}
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, &'static str> {
    if bytes.len() > FRAME_BYTES
        || bytes.last() != Some(&b'\n')
        || bytes[..bytes.len().saturating_sub(1)].contains(&b'\n')
    {
        return Err("notification_frame_incomplete");
    }
    serde_json::from_slice(bytes).map_err(|_| "notification_frame_invalid")
}

/// Host-side queue guard, not an intent ledger. Its session comes only from
/// the host's original owned service channel. Never use a generic request lane.
/// Process exit discards the guard; the service never replays unknown attempts
/// into a replacement host. Native callbacks retain their original bindings.
pub struct Guard {
    session: Session,
    highest: i64,
    pending: BTreeMap<i64, Binding>,
}
impl Guard {
    pub fn new(session: Session) -> Option<Self> {
        session.valid().then_some(Self {
            session,
            highest: 0,
            pending: BTreeMap::new(),
        })
    }
    pub fn admit(&mut self, command: &Command) -> Result<(), Refusal> {
        if !command.valid()
            || command.binding.session != self.session
            || command.binding.seq <= self.highest
        {
            return Err(Refusal::InvalidCommand);
        }
        if self.pending.len() == IN_FLIGHT {
            return Err(Refusal::Capacity);
        }
        self.highest = command.binding.seq;
        self.pending
            .insert(command.binding.seq, command.binding.clone());
        Ok(())
    }
    pub fn complete(&mut self, receipt: &Receipt) -> bool {
        if self.pending.get(&receipt.binding.seq) != Some(&receipt.binding) {
            return false;
        }
        self.pending.remove(&receipt.binding.seq);
        true
    }
    pub fn drained(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn id(n: usize) -> String {
        format!("00000000-0000-4000-8000-{n:012x}")
    }
    pub fn command(seq: i64) -> Command {
        let token = attention::encode_token(
            &json!({"host":"","name":"example","run":"run",
            "notice":id(9)})
            .as_object()
            .unwrap()
            .clone(),
        );
        Command {
            binding: Binding {
                session: Session {
                    configuration: "a".repeat(64),
                    service_epoch: id(1),
                    writer_epoch: id(2),
                    adapter: id(3),
                    host_epoch: id(4),
                    lease: id(5),
                    platform: Platform::Macos,
                },
                request_id: id(10 + seq as usize),
                intent_id: "b".repeat(64),
                revision: 1,
                seq,
            },
            action: Action::Post {
                token,
                title: "Needs attention".into(),
                body: "Example".into(),
            },
        }
    }
    #[test]
    fn closed_frames_preserve_tokens_and_refuse_truncated_or_oversize_data() {
        let original = ToHost::Command(command(1));
        let encoded = encode(&original).unwrap();
        assert_eq!(decode::<ToHost>(&encoded).unwrap(), original);
        assert!(decode::<ToHost>(&encoded[..encoded.len() - 1]).is_err());
        assert!(decode::<ToHost>(&vec![b' '; FRAME_BYTES + 1]).is_err());
        let mut value = serde_json::to_value(&original).unwrap();
        value["authority"] = true.into();
        assert!(serde_json::from_value::<ToHost>(value).is_err());
        let mut bad = command(1);
        if let Action::Post { body, .. } = &mut bad.action {
            *body = "x".repeat(FRAME_BYTES);
        }
        assert!(!bad.valid());
        assert!(encode(&ToHost::Command(bad)).is_err());
    }
    #[test]
    fn native_ids_are_platform_and_original_token_bound() {
        let command = command(1);
        let mut receipt = Receipt {
            binding: command.binding.clone(),
            outcome: Outcome::Delivered {
                native_id: NativeId::Macos {
                    request_id: attention::request_id(command.action.token()),
                },
            },
        };
        assert!(receipt.matches(&command));
        receipt.binding.session.host_epoch = id(99);
        assert!(!receipt.matches(&command));
        receipt.binding = command.binding.clone();
        receipt.outcome = Outcome::Delivered {
            native_id: NativeId::Macos {
                request_id: "wrong".into(),
            },
        };
        assert!(!receipt.matches(&command));
        assert!(!NativeId::Linux {
            bus_id: "a".repeat(32),
            owner: ":1.4".into(),
            id: 0
        }
        .valid_for(Platform::Linux, command.action.token()));
        assert!(!NativeId::Linux {
            bus_id: "a".repeat(32),
            owner: "org.example.Service".into(),
            id: 1
        }
        .valid_for(Platform::Linux, command.action.token()));
    }
    #[test]
    fn wayland_context_is_optional_bounded_and_platform_specific() {
        let command = command(1);
        let value = Activation {
            request_id: command.binding.request_id,
            token: command.action.token().into(),
            native_id: NativeId::Linux {
                bus_id: "a".repeat(32),
                owner: ":1.4".into(),
                id: 7,
            },
            platform_context: None,
        };
        let raw = encode(&value).unwrap();
        assert!(!std::str::from_utf8(&raw)
            .unwrap()
            .contains("platform_context"));
        assert_eq!(decode::<Activation>(&raw).unwrap(), value);
        let mut with_context = value;
        with_context.platform_context = Some(PlatformContext::Wayland {
            activation_token: "fixture-token".into(),
        });
        assert!(with_context.valid_for(Platform::Linux));
        assert!(!with_context.valid_for(Platform::Macos));
        assert_eq!(
            decode::<Activation>(&encode(&with_context).unwrap()).unwrap(),
            with_context
        );
        for token in [
            String::new(),
            "x".repeat(ACTIVATION_TOKEN_BYTES + 1),
            "bad\ntoken".into(),
        ] {
            with_context.platform_context = Some(PlatformContext::Wayland {
                activation_token: token,
            });
            assert!(!with_context.valid_for(Platform::Linux));
        }
    }
    #[test]
    fn replay_and_old_contexts_never_reenter_the_host_after_completion() {
        let first = command(1);
        let mut guard = Guard::new(first.binding.session.clone()).unwrap();
        guard.admit(&first).unwrap();
        assert!(guard.admit(&first).is_err());
        let receipt = Receipt {
            binding: first.binding.clone(),
            outcome: Outcome::Unknown {
                code: Uncertainty::HostLost,
            },
        };
        assert!(guard.complete(&receipt));
        assert!(guard.drained());
        assert!(guard.admit(&first).is_err());
        let mut bad = command(2);
        bad.binding.session.lease = id(99);
        assert!(guard.admit(&bad).is_err());
        for seq in 2..=9 {
            guard.admit(&command(seq)).unwrap();
        }
        assert_eq!(guard.admit(&command(10)), Err(Refusal::Capacity));
    }
}
