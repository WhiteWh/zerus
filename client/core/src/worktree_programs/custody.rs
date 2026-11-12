//! Closed DATA frames for one original host-local checkout holder. The private
//! native owner supplies admission and actual-join provenance; these frames,
//! process fields and descriptor deliveries never grant stop/Force authority.
use super::{stop::Input, Discovery, Manifest};
use crate::creation::canonical_uuid;
use serde::{Deserialize, Serialize};

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use crate::run::worktree_custody::{Error, Owner};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod clock;

pub const ABI: &str = "worktree-program-custody-v1";
pub const HELP_MARKER: &str = "Worktree program custody ABI: worktree-program-custody-v1";
pub const FRAME_BYTES: usize = super::stop::INPUT_LIMIT + 4096;
pub const PHASE_MILLIS: u64 = 15_000;
pub const MAX_PHASES: usize = 17;
pub const RETENTION_SECONDS: u64 = 300;

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub pipeline: String,
    pub manifest: String,
    pub request: String,
    pub input: String,
}
impl Binding {
    pub fn for_cleanup(pipeline: &str, input: &super::cleanup::Input) -> Self {
        Self {
            pipeline: pipeline.into(),
            manifest: input.stop.manifest.binding(),
            request: input.request_id.clone(),
            input: input.binding(),
        }
    }
    pub fn for_input(pipeline: &str, input: &Input) -> Self {
        Self {
            pipeline: pipeline.into(),
            manifest: input.manifest.binding(),
            request: input.request_id.clone(),
            input: input.binding(),
        }
    }
    pub fn valid(&self) -> bool {
        canonical_uuid(&self.pipeline)
            && canonical_uuid(&self.request)
            && self.pipeline != self.request
            && hash(&self.manifest)
            && hash(&self.input)
    }
    pub fn bytes(&self) -> Result<Vec<u8>, &'static str> {
        if !self.valid() {
            return Err("program_custody_binding_invalid");
        }
        serde_json::to_vec(self).map_err(|_| "program_custody_binding_invalid")
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > 4096 {
            return Err("program_custody_binding_oversized");
        }
        let binding: Self =
            serde_json::from_slice(bytes).map_err(|_| "program_custody_binding_invalid")?;
        binding
            .valid()
            .then_some(binding)
            .ok_or("program_custody_binding_invalid")
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeChild {
    pub pid: u32,
    pub parent: u32,
    pub birth: String,
    /// Mac: the exact token from the native own-original-child code check.
    /// Linux uses its original executing object and has no audit token. These
    /// serialized words are DATA; the private owner supplies their provenance.
    pub audit: Option<[u32; 8]>,
}
impl NativeChild {
    pub fn valid(&self) -> bool {
        self.pid > 0
            && (self.parent > 0 || self.pid == 1 && self.parent == 0)
            && self.parent != self.pid
            && !self.birth.is_empty()
            && self.birth.len() <= 128
            && !self.birth.chars().any(char::is_control)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Open {
        pipeline: String,
        discovery: Discovery,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        presentation: bool,
    },
    Arm {
        pipeline: String,
        input: Box<Input>,
        borrower: NativeChild,
        deadline_millis: u64,
    },
    ArmCleanup {
        pipeline: String,
        input: Box<super::cleanup::Input>,
        borrower: NativeChild,
        deadline_millis: u64,
    },
    Observe {
        pipeline: String,
        observation: String,
    },
    Joined {
        binding: Binding,
    },
    Close {
        pipeline: String,
    },
}
impl Control {
    pub fn pipeline(&self) -> &str {
        match self {
            Self::Open { pipeline, .. }
            | Self::Arm { pipeline, .. }
            | Self::ArmCleanup { pipeline, .. }
            | Self::Observe { pipeline, .. }
            | Self::Close { pipeline } => pipeline,
            Self::Joined { binding } => &binding.pipeline,
        }
    }
    pub fn valid(&self) -> bool {
        canonical_uuid(self.pipeline())
            && match self {
                Self::Open { discovery, .. } => super::capture_valid(&discovery.capture),
                Self::Arm {
                    pipeline,
                    input,
                    borrower,
                    deadline_millis,
                } => {
                    input.valid()
                        && borrower.valid()
                        && *deadline_millis > 0
                        && Binding::for_input(pipeline, input).valid()
                }
                Self::Joined { binding } => binding.valid(),
                Self::ArmCleanup {
                    pipeline,
                    input,
                    borrower,
                    deadline_millis,
                } => {
                    input.valid()
                        && borrower.valid()
                        && *deadline_millis > 0
                        && Binding::for_cleanup(pipeline, input).valid()
                }
                Self::Observe {
                    pipeline,
                    observation,
                } => canonical_uuid(observation) && observation != pipeline,
                Self::Close { .. } => true,
            }
    }
    pub fn encode(&self) -> Result<Vec<u8>, &'static str> {
        if !self.valid() {
            return Err("program_custody_control_invalid");
        }
        let mut bytes = serde_json::to_vec(self).map_err(|_| "program_custody_control_invalid")?;
        bytes.push(b'\n');
        if bytes.len() > FRAME_BYTES {
            return Err("program_custody_control_oversized");
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > FRAME_BYTES {
            return Err("program_custody_control_oversized");
        }
        let frame: Self =
            serde_json::from_slice(bytes).map_err(|_| "program_custody_control_invalid")?;
        frame
            .valid()
            .then_some(frame)
            .ok_or("program_custody_control_invalid")
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Ready {
        abi: String,
        pipeline: String,
        manifest: Box<Manifest>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        presentation: Option<super::presentation::Presentation>,
        holder: NativeChild,
        socket: String,
    },
    Armed {
        binding: Binding,
    },
    Observed {
        pipeline: String,
        observation: String,
        manifest: Box<Manifest>,
    },
    Retired {
        binding: Binding,
    },
    Closed {
        pipeline: String,
    },
}
impl Reply {
    pub fn valid(&self, now: u64) -> bool {
        match self {
            Self::Ready {
                abi,
                pipeline,
                manifest,
                holder,
                socket,
                ..
            } => {
                abi == ABI
                    && canonical_uuid(pipeline)
                    && pipeline != &manifest.id
                    && manifest.valid(now)
                    && holder.valid()
                    && socket.len() <= 100
                    && socket.starts_with('/')
                    && !socket.chars().any(char::is_control)
            }
            Self::Armed { binding } | Self::Retired { binding } => binding.valid(),
            Self::Observed {
                pipeline,
                observation,
                manifest,
            } => {
                canonical_uuid(pipeline)
                    && canonical_uuid(observation)
                    && pipeline != observation
                    && pipeline != &manifest.id
                    && manifest.valid(now)
            }
            Self::Closed { pipeline } => canonical_uuid(pipeline),
        }
    }
    pub fn encode(&self, now: u64) -> Result<Vec<u8>, &'static str> {
        if !self.valid(now) {
            return Err("program_custody_reply_invalid");
        }
        let mut bytes = serde_json::to_vec(self).map_err(|_| "program_custody_reply_invalid")?;
        bytes.push(b'\n');
        if bytes.len() > FRAME_BYTES {
            return Err("program_custody_reply_oversized");
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], now: u64) -> Result<Self, &'static str> {
        if bytes.len() > FRAME_BYTES {
            return Err("program_custody_reply_oversized");
        }
        let bytes = super::presentation::sanitize_reply(bytes)
            .map_err(|_| "program_custody_reply_invalid")?;
        let frame: Self =
            serde_json::from_slice(&bytes).map_err(|_| "program_custody_reply_invalid")?;
        frame
            .valid(now)
            .then_some(frame)
            .ok_or("program_custody_reply_invalid")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custody_data_is_closed_bounded_and_never_a_drain_or_force_claim() {
        let pipeline = "99999999-9999-4999-8999-999999999999";
        let open = Control::Open {
            presentation: false,
            pipeline: pipeline.into(),
            discovery: Discovery {
                capture: super::super::tests::manifest().capture,
            },
        };
        assert_eq!(Control::decode(&open.encode().unwrap()).unwrap(), open);
        let mut raw = serde_json::to_value(&open).unwrap();
        raw["force"] = true.into();
        assert!(Control::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
        assert!(Control::decode(&vec![0; FRAME_BYTES + 1]).is_err());
        let input = super::super::stop::tests::input();
        let binding = Binding::for_input(pipeline, &input);
        assert!(binding.valid());
        assert_eq!(Binding::decode(&binding.bytes().unwrap()).unwrap(), binding);
        let mut raw = serde_json::to_value(&binding).unwrap();
        raw["drained"] = true.into();
        assert!(Binding::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
        assert!(!Control::Arm {
            pipeline: pipeline.into(),
            input: Box::new(input.clone()),
            borrower: NativeChild {
                pid: 2,
                parent: 3,
                birth: "native".into(),
                audit: None,
            },
            deadline_millis: 0
        }
        .valid());
        let arm = Control::Arm {
            pipeline: pipeline.into(),
            input: Box::new(input.clone()),
            borrower: NativeChild {
                pid: 2,
                parent: 3,
                birth: "native".into(),
                audit: None,
            },
            deadline_millis: 1,
        };
        assert_eq!(Control::decode(&arm.encode().unwrap()).unwrap(), arm);
        let ready = Reply::Ready {
            presentation: None,
            abi: ABI.into(),
            pipeline: pipeline.into(),
            manifest: Box::new(input.manifest.clone()),
            holder: NativeChild {
                pid: 2,
                parent: 3,
                birth: "native".into(),
                audit: None,
            },
            socket: "/tmp/example/original".into(),
        };
        assert_eq!(
            Reply::decode(&ready.encode(1000).unwrap(), 1000).unwrap(),
            ready
        );
        assert!(Reply::decode(&ready.encode(1000).unwrap(), 1300).is_err());
        // Old peers get the byte-compatible old Open/Ready shapes. New peers
        // negotiate presentation only through the same selected helper help.
        assert!(serde_json::to_value(&open)
            .unwrap()
            .get("presentation")
            .is_none());
        let old = serde_json::to_value(&ready).unwrap();
        assert!(old.get("presentation").is_none());
        for raw in [
            serde_json::json!(true),
            serde_json::json!({"abi":"unknown"}),
            serde_json::json!({"unexpected":"field"}),
        ] {
            let mut value = old.clone();
            value["presentation"] = raw;
            let decoded = Reply::decode(&serde_json::to_vec(&value).unwrap(), 1000).unwrap();
            assert_eq!(decoded, ready);
        }
        let display = super::super::presentation::Presentation::new(
            &input.manifest,
            super::super::presentation::Identity {
                pid: 10,
                birth: "agent-birth".into(),
            },
            super::super::presentation::Identity {
                pid: 20,
                birth: "supervisor-birth".into(),
            },
        );
        let mut value = old.clone();
        value["presentation"] = serde_json::to_value(&display).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        let Reply::Ready {
            presentation,
            manifest,
            ..
        } = Reply::decode(encoded.as_bytes(), 1000).unwrap()
        else {
            panic!("Ready")
        };
        assert_eq!(presentation, Some(display));
        assert_eq!(*manifest, input.manifest);
        // Duplicate keys in the optional companion do not select identities.
        let duplicate = encoded.replace("\"agent\":{", "\"agent\":null,\"agent\":{");
        let Reply::Ready { presentation, .. } = Reply::decode(duplicate.as_bytes(), 1000).unwrap()
        else {
            panic!("Ready")
        };
        assert!(presentation.is_none());

        let observation = "88888888-8888-4888-8888-888888888888";
        let observe = Control::Observe {
            pipeline: pipeline.into(),
            observation: observation.into(),
        };
        assert_eq!(
            Control::decode(&observe.encode().unwrap()).unwrap(),
            observe
        );
        let mut current = input.manifest.clone();
        current.id = "77777777-7777-4777-8777-777777777777".into();
        let observed = Reply::Observed {
            pipeline: pipeline.into(),
            observation: observation.into(),
            manifest: Box::new(current),
        };
        assert_eq!(
            Reply::decode(&observed.encode(1100).unwrap(), 1100).unwrap(),
            observed
        );
        let mut raw = serde_json::to_value(&observe).unwrap();
        raw["force"] = true.into();
        assert!(Control::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
        let mut changed = input;
        changed.original.push('\n');
        assert_ne!(binding, Binding::for_input(pipeline, &changed));
    }

    #[test]
    fn native_child_accepts_namespace_init_and_rejects_invalid_zero_identities() {
        let child = |pid, parent| NativeChild {
            pid,
            parent,
            birth: "native".into(),
            audit: None,
        };
        assert!(child(1, 0).valid());
        assert!(child(1, 2).valid());
        assert!(!child(0, 1).valid());
        assert!(!child(2, 0).valid());
    }
}
