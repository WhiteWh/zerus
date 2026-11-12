//! Optional native PRESENTATION data. Never part of Manifest or any effect
//! binding. Missing or inconsistent data leaves the complete list visible.
use super::{Manifest, Program};
use serde::{Deserialize, Serialize};

pub const ABI: &str = "worktree-program-presentation-v1";
pub const HELP_MARKER: &str = "Worktree program presentation ABI: worktree-program-presentation-v1";
const MAX_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub pid: u32,
    pub birth: String,
}
impl Identity {
    fn valid(&self) -> bool {
        self.pid > 0 && super::bounded(&self.birth, 128)
    }
    fn matches(&self, program: &Program) -> bool {
        self.pid == program.pid && self.birth == program.birth
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    pub abi: String,
    pub manifest: String,
    pub agent: Identity,
    pub supervisor: Identity,
}
impl Presentation {
    /// The native discoverer supplies its bracketed record-bound pair. These
    /// serialized fields confer no signal, cleanup or consent authority.
    pub fn new(manifest: &Manifest, agent: Identity, supervisor: Identity) -> Self {
        Self {
            abi: ABI.into(),
            manifest: manifest.binding(),
            agent,
            supervisor,
        }
    }
    fn valid_for(&self, manifest: &Manifest) -> bool {
        self.abi == ABI
            && self.manifest == manifest.binding()
            && self.agent.valid()
            && self.supervisor.valid()
            && self.agent.pid != self.supervisor.pid
            && [&self.agent, &self.supervisor].iter().all(|own| {
                manifest
                    .programs
                    .iter()
                    .all(|program| program.pid != own.pid || own.matches(program))
            })
    }
    /// Initial dialog ONLY. Execution and Force must use Manifest.programs.
    pub fn visible<'a>(presentation: Option<&Self>, manifest: &'a Manifest) -> Vec<&'a Program> {
        let own = presentation.filter(|value| value.valid_for(manifest));
        manifest
            .programs
            .iter()
            .filter(|program| {
                !own.is_some_and(|value| {
                    value.agent.matches(program) || value.supervisor.matches(program)
                })
            })
            .collect()
    }
}

/// Sanitize ONLY the optional top-level companion before the strict tagged
/// custody decoder. RawValue preserves duplicate keys inside this companion;
/// serde's internally-tagged enum buffering otherwise loses that information.
/// All required custody fields are decoded unchanged, with their usual closed
/// schema and duplicate-field checks. The caller bounds the whole frame first.
pub(super) fn sanitize_reply(
    bytes: &[u8],
) -> Result<std::borrow::Cow<'_, [u8]>, serde_json::Error> {
    #[derive(Deserialize)]
    struct Envelope<'a> {
        #[serde(borrow, default)]
        presentation: Option<&'a serde_json::value::RawValue>,
    }
    let envelope: Envelope<'_> = serde_json::from_slice(bytes)?;
    if let Some(raw) = envelope.presentation {
        if raw.get().len() > MAX_BYTES || serde_json::from_str::<Presentation>(raw.get()).is_err() {
            // RawValue borrows this exact original buffer, including its span.
            let start = raw.get().as_ptr() as usize - bytes.as_ptr() as usize;
            let mut sanitized = Vec::with_capacity(bytes.len());
            sanitized.extend_from_slice(&bytes[..start]);
            sanitized.extend_from_slice(b"null");
            sanitized.extend_from_slice(&bytes[start + raw.get().len()..]);
            return Ok(std::borrow::Cow::Owned(sanitized));
        }
    }
    Ok(std::borrow::Cow::Borrowed(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Manifest, Presentation) {
        let mut manifest = super::super::tests::manifest();
        manifest.programs = [
            (10, "agent"),
            (20, "wrapper"),
            (30, "agent"),
            (40, "descendant"),
        ]
        .into_iter()
        .map(|(pid, label)| Program {
            pid,
            birth: format!("birth-{pid}"),
            label: label.into(),
        })
        .collect();
        let identity = |pid| Identity {
            pid,
            birth: format!("birth-{pid}"),
        };
        let display = Presentation::new(&manifest, identity(10), identity(20));
        (manifest, display)
    }
    fn pids(manifest: &Manifest, display: Option<&Presentation>) -> Vec<u32> {
        Presentation::visible(display, manifest)
            .iter()
            .map(|p| p.pid)
            .collect()
    }
    #[test]
    fn only_exact_pair_is_hidden_without_changing_any_execution_data() {
        let (manifest, display) = fixture();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let binding = manifest.binding();
        assert_eq!(pids(&manifest, Some(&display)), [30, 40]);
        assert_eq!(pids(&manifest, None), [10, 20, 30, 40]);
        assert_eq!(serde_json::to_vec(&manifest).unwrap(), bytes);
        assert_eq!(manifest.binding(), binding);
        assert_eq!(manifest.programs.len(), 4);
    }
    #[test]
    fn changed_capture_owner_manifest_birth_or_duplicate_role_stays_visible() {
        let (manifest, display) = fixture();
        for change in 0..6 {
            let mut value = display.clone();
            let mut current = manifest.clone();
            match change {
                0 => value.agent.birth = "reused".into(),
                1 => value.supervisor = value.agent.clone(),
                2 => current.capture.target.run_id = "other-run".into(),
                3 => current.capture.owner_id = "other-owner".into(),
                4 => current.programs[3].label = "changed".into(),
                _ => value.abi = "unknown".into(),
            }
            assert_eq!(pids(&current, Some(&value)), [10, 20, 30, 40]);
        }
    }

    #[test]
    fn namespace_init_identities_stay_valid_but_pid_zero_is_refused() {
        let (mut manifest, _) = fixture();
        manifest.programs = vec![
            Program {
                pid: 1,
                birth: "birth-1".into(),
                label: "agent".into(),
            },
            Program {
                pid: 2,
                birth: "birth-2".into(),
                label: "supervisor".into(),
            },
        ];
        let identity = |pid| Identity {
            pid,
            birth: format!("birth-{pid}"),
        };
        let display = Presentation::new(&manifest, identity(1), identity(2));
        assert_eq!(pids(&manifest, Some(&display)), Vec::<u32>::new());
        let mut invalid = display;
        invalid.agent.pid = 0;
        assert_eq!(pids(&manifest, Some(&invalid)), [1, 2]);
    }
}
