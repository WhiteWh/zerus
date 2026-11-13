//! Portable, read-only conversation references. Aliases route; machine IDs verify.
use crate::{
    cli::{self, Error},
    config::Config,
    state, swarm,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug)]
pub(crate) struct Reference {
    pub machine: String,
    pub host: String,
    pub name: String,
    pub conversation: String,
    pub run: String,
    pub archive: String,
    pub agent: String,
    /// The durable session ID (zerus-ucck P1c). Parsed and verified when a
    /// reference carries it; emission needs the P3 fleet transition gate,
    /// because older parsers refuse an unknown field.
    pub session: String,
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn decode(value: &str) -> cli::Result<String> {
    let mut bytes = Vec::new();
    let mut input = value.bytes();
    while let Some(b) = input.next() {
        if b == b'%' {
            let a = input.next().and_then(|v| (v as char).to_digit(16));
            let c = input.next().and_then(|v| (v as char).to_digit(16));
            let (a, c) = a
                .zip(c)
                .ok_or_else(|| Error::new(1, "Invalid session reference escape"))?;
            bytes.push((a * 16 + c) as u8);
        } else if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            bytes.push(b);
        } else {
            return Err(Error::new(1, "Invalid session reference encoding"));
        }
    }
    String::from_utf8(bytes).map_err(|_| Error::new(1, "Invalid session reference text"))
}
impl Reference {
    pub fn parse(uri: &str) -> cli::Result<Self> {
        let query = uri
            .strip_prefix("zerus://session?")
            .filter(|_| uri.len() <= 8192)
            .ok_or_else(|| Error::new(1, "Invalid Zerus session reference"))?;
        let mut fields = BTreeMap::new();
        for pair in query.split('&') {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| Error::new(1, "Invalid session reference field"))?;
            let key = decode(key)?;
            if ![
                "machine",
                "host",
                "name",
                "conversation",
                "run",
                "archive",
                "agent",
                "session",
            ]
            .contains(&key.as_str())
                || fields.insert(key, decode(value)?).is_some()
            {
                return Err(Error::new(
                    1,
                    "Unknown or duplicate session reference field",
                ));
            }
        }
        let take = |key: &str| fields.get(key).cloned().unwrap_or_default();
        let reference = Self {
            machine: take("machine"),
            host: take("host"),
            name: take("name"),
            conversation: take("conversation"),
            run: take("run"),
            archive: take("archive"),
            agent: take("agent"),
            session: take("session"),
        };
        if !uuid::Uuid::parse_str(&reference.machine)
            .is_ok_and(|id| !id.is_nil() && id.to_string() == reference.machine)
            || reference.host.is_empty()
            || reference.host.len() > 80
            || reference.host.starts_with('-')
            || !reference
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            || reference.name.is_empty()
            || reference.name.len() > 512
            || !reference.name.contains('/')
            || (reference.conversation.is_empty() && reference.run.is_empty())
            || (!reference.session.is_empty() && !state::session_id::canonical(&reference.session))
            || [
                &reference.name,
                &reference.conversation,
                &reference.run,
                &reference.archive,
                &reference.agent,
            ]
            .iter()
            .any(|v| v.len() > 512 || v.chars().any(char::is_control))
        {
            return Err(Error::new(1, "Invalid session reference identity"));
        }
        Ok(reference)
    }
    fn uri(&self) -> String {
        let fields = [
            ("machine", &self.machine),
            ("host", &self.host),
            ("name", &self.name),
            ("conversation", &self.conversation),
            ("run", &self.run),
            ("archive", &self.archive),
            ("agent", &self.agent),
            ("session", &self.session),
        ];
        format!(
            "zerus://session?{}",
            fields
                .iter()
                .filter(|(_, v)| !v.is_empty())
                .map(|(k, v)| format!("{k}={}", encode(v)))
                .collect::<Vec<_>>()
                .join("&")
        )
    }
}
pub(crate) fn create(config: &Config, args: &[String]) -> cli::Result<i32> {
    let name = args.first().ok_or_else(|| {
        Error::new(
            1,
            "usage: hgs reference <session> --json [--archive ID] [--agent ID]",
        )
    })?;
    let mut archive = String::new();
    let mut agent = String::new();
    let mut expected_run = None;
    let mut expected_conversation = None;
    let mut as_json = false;
    let mut index = 1;
    while index < args.len() {
        if args[index] == "--json" && !as_json {
            as_json = true;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| Error::new(1, "Missing reference option value"))?;
        match args[index].as_str() {
            "--archive" if archive.is_empty() => archive = value.clone(),
            "--agent" if agent.is_empty() => agent = value.clone(),
            "--run" if expected_run.is_none() => expected_run = Some(value.as_str()),
            "--conversation" if expected_conversation.is_none() => {
                expected_conversation = Some(value.as_str())
            }
            _ => return Err(Error::new(1, "Invalid reference option")),
        }
        index += 2;
    }
    let details = state::reference_details(name, &archive, "").map_err(|e| Error::new(1, e))?;
    let text = |key: &str| details[key].as_str().unwrap_or_default().to_owned();
    if expected_run.is_some_and(|id| id != text("run_id"))
        || expected_conversation.is_some_and(|id| id != text("conversation_id"))
    {
        return Err(Error::new(
            1,
            "Session changed before copying its reference; refresh it",
        ));
    }
    if !agent.is_empty() {
        let child =
            state::reference_details(name, &archive, &agent).map_err(|e| Error::new(1, e))?;
        if child["parent_conversation_id"] != details["conversation_id"] {
            return Err(Error::new(
                1,
                "Parent conversation changed before copying its child reference",
            ));
        }
    }
    let reference = Reference {
        machine: swarm::local_node_id(config).map_err(|e| Error::new(1, e.to_string()))?,
        host: config.host().into(),
        name: name.clone(),
        conversation: text("conversation_id"),
        run: text("run_id"),
        archive,
        agent,
        session: emitted_session(config, &details),
    };
    print(&reference, as_json, json!({}))
}

/// The optional reference field stays absent until every answering route is
/// compatible. This is metadata only; the receiver verifies all selectors.
fn emitted_session(config: &Config, record: &Value) -> String {
    record["session_id"]
        .as_str()
        .filter(|id| {
            state::session_id::canonical(id)
                && uuid::Uuid::parse_str(id).is_ok_and(|id| !id.is_nil())
        })
        .filter(|_| swarm::session_labels::cached(config))
        .unwrap_or_default()
        .to_owned()
}

fn print(reference: &Reference, as_json: bool, extra: Value) -> cli::Result<i32> {
    let uri = reference.uri();
    Reference::parse(&uri)?;
    if as_json {
        let mut output = json!({"schema":1,"uri":uri,"name":reference.name,"machine_id":reference.machine,
        "host":reference.host,"run_id":reference.run,"conversation_id":reference.conversation,"archive_id":reference.archive,"agent_id":reference.agent});
        if !reference.session.is_empty() {
            output["session_id"] = json!(reference.session);
        }
        for (key, value) in extra.as_object().into_iter().flatten() {
            output[key] = value.clone();
        }
        println!("{output}");
    } else {
        println!("{uri}");
    }
    Ok(0)
}

/// `hgs self`: the reference of the agent whose process runs this CLI, under
/// its current name. Verified native ancestry identifies the caller, or for a
/// DeepSeek tool command (task #34) the binding of its DSH_SESSION_ID. Native
/// shell claims are explicitly distinguished from process proof and never enter
/// caller(), the send/graph attribution path.
pub(crate) fn current(config: &Config, args: &[String]) -> cli::Result<i32> {
    let as_json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return Err(Error::new(1, "usage: hgs self [--json]")),
    };
    let (record, source) = match state::session_graph::caller_record() {
        Some(record) => (record, "process"),
        // A native claim stays a claim. Without one, a command under a
        // recorded DeepSeek host says so (zerus-ppd26.15).
        None => (state::parent_link::native_self().map_err(|e| Error::new(1, e))?
            .ok_or_else(|| match state::dsh::host_refusal() {
                Some(refusal) => Error::new(1, format!("hgs self: {refusal}")),
                None => Error::new(1, "hgs self: no live Zerus session owns this process"),
            })?, "claimed"),
    };
    let (reference, provider) = from_record(config, &record)?;
    let parent = json!({"session_id":record["session_id"],"name":record["name"],"run_id":record["run_id"],"conversation_id":record["conversation_id"]});
    print(&reference, as_json, json!({"provider":provider,"identity_source":source,"session_id":record["session_id"],"parent_identity":parent}))
}

/// The reference and provider of the live agent whose process runs this CLI,
/// under its current name; `None` when no agent's ancestry owns it.
fn caller(config: &Config) -> cli::Result<Option<(Reference, String)>> {
    let Some(record) = self_record() else {
        return Ok(None);
    };
    from_record(config, &record).map(Some)
}

fn from_record(config: &Config, record: &Value) -> cli::Result<(Reference, String)> {
    let text = |key: &str| record[key].as_str().unwrap_or_default().to_owned();
    let reference = Reference {
        machine: swarm::local_node_id(config).map_err(|e| Error::new(1, e.to_string()))?,
        host: config.host().into(),
        name: text("name"),
        conversation: text("conversation_id"),
        run: text("run_id"),
        archive: String::new(),
        agent: String::new(),
        session: emitted_session(config, record),
    };
    let provider = record["agent"].as_str().filter(|s| !s.is_empty())
        .or_else(|| record["backend"].as_str()).unwrap_or_default().to_owned();
    Ok((reference, provider))
}

/// The calling agent's record. Task #34: a DeepSeek tool command has no hgs
/// session environment and runs under the shared DeepSeek host, so ancestry
/// never names it ("hgs self: no live Zerus session owns this process"); its
/// harness exports the conversation as DSH_SESSION_ID, whose binding is the
/// session, as `hgs run` and `hgs team` take it.
fn self_record() -> Option<serde_json::Value> {
    state::session_graph::caller_record().or_else(state::dsh_caller_record)
}

/// The text a guarded send of the calling agent delivers: `text` inside one
/// envelope naming the sending session, so the recipient can tell another
/// agent declared it and inspect or answer that session. The same text and
/// unchanged sender metadata give the same bytes. A rename or resumed run
/// changes that metadata: an old request ID then fails the ordinary content
/// guard, rather than gaining permission to resend uncertain input.
pub(crate) fn sender_envelope(text: &str) -> Option<String> {
    // Leave an oversized body to the unchanged input validator; do not expand
    // a rejected multi-megabyte JSON text into XML first.
    if text.len() > 64 * 1024 {
        return None;
    }
    let config = Config::load().ok()?;
    let (reference, _) = caller(&config).ok()??;
    let uri = reference.uri();
    Reference::parse(&uri).ok()?;
    Some(envelope(&reference.name, &reference.host, &uri, text))
}

fn envelope(name: &str, host: &str, uri: &str, text: &str) -> String {
    let body = |value: &str| value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let attribute = |value: &str| body(value).replace('"', "&quot;");
    format!(
        "<zerus-message from=\"{}\" host=\"{}\" reference=\"{}\">\n{}\n</zerus-message>",
        attribute(name),
        attribute(host),
        attribute(uri),
        body(text)
    )
}

/// Decode only the complete, bounded XML subset hgs emits. The metadata is a
/// declaration, not authentication: anyone can type this frame. Never use it
/// as authority to route input or bypass the ordinary target/delivery guards.
pub(crate) fn sender_message(text: &str) -> Option<(String, String, String)> {
    if text.len() > 64 * 1024 {
        return None;
    }
    fn attribute(rest: &str) -> Option<(String, &str)> {
        let (value, rest) = rest.split_once('"')?;
        Some((xml_text(value)?, rest))
    }
    let rest = text.strip_prefix("<zerus-message from=\"")?;
    let (name, rest) = attribute(rest)?;
    let (host, rest) = attribute(rest.strip_prefix(" host=\"")?)?;
    let (uri, rest) = attribute(rest.strip_prefix(" reference=\"")?)?;
    let body = rest.strip_prefix(">\n")?.strip_suffix("\n</zerus-message>")?;
    let reference = Reference::parse(&uri).ok()?;
    if reference.name != name || reference.host != host {
        return None;
    }
    Some((name, uri, xml_text(body)?))
}

/// Named entities only, decoded once. No DTD, numeric references, nested
/// markup, bare ampersands or invalid XML characters can enter the text.
fn xml_text(value: &str) -> Option<String> {
    let mut decoded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find('&') {
        let plain = &rest[..at];
        if !xml_plain(plain) {
            return None;
        }
        decoded.push_str(plain);
        let entity = &rest[at..];
        let (encoded, character) = [("&amp;", '&'), ("&lt;", '<'), ("&gt;", '>'), ("&quot;", '"')]
            .into_iter()
            .find(|(encoded, _)| entity.starts_with(*encoded))?;
        decoded.push(character);
        rest = &entity[encoded.len()..];
    }
    if !xml_plain(rest) {
        return None;
    }
    decoded.push_str(rest);
    Some(decoded)
}

fn xml_plain(value: &str) -> bool {
    !value.contains('<') && value.chars().all(|character| {
        matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
    })
}
pub(crate) fn inspect(config: &Config, reference: &Reference) -> cli::Result<Value> {
    if swarm::local_node_id(config).map_err(|e| Error::new(1, e.to_string()))? != reference.machine
    {
        return Err(Error::new(
            1,
            "Session reference belongs to another computer; use a configured @host",
        ));
    }
    state::inspect_reference(reference).map_err(|e| Error::new(1, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task #34 (audit #29 P6): `hgs self` from a DeepSeek tool command said
    /// "no live Zerus session owns this process": it has no hgs session
    /// environment and no hgs process above it. The binding of its
    /// DSH_SESSION_ID is its session now.
    #[test]
    fn hgs_self_takes_a_deepseek_tool_commands_binding() {
        let root = tempfile::tempdir().unwrap();
        state::with_test_state_root(root.path(), || {
            if state::session_graph::caller_record().is_some() {
                return; // run inside a real session: its own record answers first
            }
            assert!(self_record().is_none(), "no session and no DeepSeek conversation");
            state::builds_test_support::write_qa_binding();
            state::builds_test_support::set_dsh_session(Some("session-10e9dd92-4a7a-49be-a139-da281115f842"));
            let found = self_record();
            state::builds_test_support::set_dsh_session(None);
            let found = found.expect("the DeepSeek binding is the caller");
            assert_eq!(found["name"], "dsh/Zerus/zerus-next-for-the-fork-qa");
            assert_eq!(found["run_id"], "ba3d8c92-041c-4e14-81b2-b2e63f0e7ee7");
        });
    }

    #[test]
    fn session_emission_needs_a_fresh_open_gate_and_a_canonical_stored_id() {
        use std::{
            collections::BTreeMap,
            time::{Duration, Instant},
        };
        let root = tempfile::tempdir().unwrap();
        let config = Config {
            home: root.path().to_string_lossy().into(),
            dir: root.path().join("config"),
            state: root.path().join("state"),
            selves: vec!["fixture".into()],
            peers: vec![],
            base_peers: vec![],
            peers_from_environment: false,
            machines: BTreeMap::new(),
            tab: false,
            tab_colors: String::new(),
        };
        for path in [&config.dir, &config.state, &config.dir.join("swarm")] {
            crate::platform::private_dir(path).unwrap();
        }
        let machine = "11111111-1111-4111-8111-111111111111";
        let sid = "22222222-2222-4222-8222-222222222222";
        let raw = json!({"node_id":machine,"catalog":{"schema":1,"swarm_id":"33333333-3333-4333-8333-333333333333","operations":{}},"imported":true,"bindings":{},"peers":{}});
        std::fs::write(config.dir.join("swarm/catalog.json"), raw.to_string()).unwrap();
        let record = json!({"session_id":sid});
        assert_eq!(emitted_session(&config, &record), "");
        swarm::session_labels::refresh_local(&config, Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert_eq!(emitted_session(&config, &record), sid);
        for id in ["", "not-a-uuid", "00000000-0000-0000-0000-000000000000"] {
            assert_eq!(emitted_session(&config, &json!({"session_id":id})), "");
        }
        let reference = Reference {
            machine: machine.into(),
            host: "fixture".into(),
            name: "claude/example/shared".into(),
            conversation: "conversation".into(),
            run: "run".into(),
            archive: String::new(),
            agent: String::new(),
            session: emitted_session(&config, &record),
        };
        let uri = reference.uri();
        assert_eq!(Reference::parse(&uri).unwrap().session, sid);
        let message = envelope(&reference.name, &reference.host, &uri, "hello");
        let (_, copied, _) = sender_message(&message).unwrap();
        assert_eq!(Reference::parse(&copied).unwrap().session, sid);
        std::fs::write(
            config.dir.join("swarm/session-id-memberships.json"),
            b"damaged primary",
        )
        .unwrap();
        assert_eq!(emitted_session(&config, &record), "");
    }
    #[test]
    fn strict_uri_round_trip_and_ambiguous_fields() {
        let reference = Reference {
            machine: "11111111-1111-4111-8111-111111111111".into(),
            host: "fixture".into(),
            name: "claude/project/a & б".into(),
            conversation: "conversation".into(),
            run: "run".into(),
            archive: String::new(),
            agent: String::new(),
            session: String::new(),
        };
        let uri = reference.uri();
        assert_eq!(Reference::parse(&uri).unwrap().name, reference.name);
        for tail in [
            "&machine=other",
            "&%6Eame=other",
            "&secret=x",
            "#fragment",
            "&agent=%00",
            "&agent=%ZZ",
            "&agent=%FF",
            "&",
        ] {
            assert!(Reference::parse(&(uri.clone() + tail)).is_err(), "{tail}");
        }
    }

    /// zerus-ucck P1c: references do not emit `session` until every reader
    /// parses it. One built as `hgs reference` and `hgs self` build it uses
    /// only the fields the strict parser of older hgs accepts, and still parses.
    #[test]
    fn an_emitted_reference_keeps_the_strict_shape_older_parsers_accept() {
        const OLDER: [&str; 7] = ["machine", "host", "name", "conversation", "run", "archive", "agent"];
        let reference = Reference {
            machine: "11111111-1111-4111-8111-111111111111".into(),
            host: "fixture".into(),
            name: "claude/project/a".into(),
            conversation: "conversation".into(),
            run: "run".into(),
            archive: "22222222-2222-4222-8222-222222222222".into(),
            agent: "agent".into(),
            session: String::new(),
        };
        let uri = reference.uri();
        let query = uri.strip_prefix("zerus://session?").unwrap();
        for pair in query.split('&') {
            let key = pair.split_once('=').unwrap().0;
            assert!(OLDER.contains(&key), "{key} in {uri}");
        }
        assert_eq!(Reference::parse(&uri).unwrap().session, "");
        // The current parser accepts a session ID, exactly once and canonical.
        let session = "3f2a9c1d-0b4e-4d6a-9c1d-2e3f4a5b6c7d";
        assert_eq!(Reference::parse(&format!("{uri}&session={session}")).unwrap().session, session);
        for tail in [
            "&session=NOT-A-UUID".to_owned(),
            format!("&session={}", session.to_uppercase()),
            format!("&session={session}&session={session}"),
        ] {
            assert!(Reference::parse(&format!("{uri}{tail}")).is_err(), "{tail}");
        }
    }

    #[test]
    fn the_sender_envelope_names_an_exact_copyable_reference() {
        let reference = Reference {
            machine: "11111111-1111-4111-8111-111111111111".into(),
            host: "arch".into(),
            name: "claude/p/\"<review>\" & co".into(),
            conversation: "conversation".into(),
            run: "run".into(),
            archive: String::new(),
            agent: String::new(),
            session: String::new(),
        };
        let uri = reference.uri();
        let text = envelope(&reference.name, &reference.host, &uri, "Line one.\n\nLine two.");
        let encoded_uri = uri.replace('&', "&amp;");
        assert_eq!(text, format!("<zerus-message from=\"claude/p/&quot;&lt;review&gt;&quot; &amp; co\" \
            host=\"arch\" reference=\"{encoded_uri}\">\nLine one.\n\nLine two.\n</zerus-message>"));
        let (sender, copied, body) = sender_message(&text).unwrap();
        assert_eq!(Reference::parse(&copied).unwrap().name, reference.name);
        assert_eq!((sender, copied, body), (reference.name.clone(), uri.clone(), "Line one.\n\nLine two.".into()));
        assert_eq!(envelope(&reference.name, &reference.host, &uri, ""), envelope(&reference.name, "arch", &uri, ""));
    }

    #[test]
    fn sender_xml_preserves_literal_text_without_expanding_it_twice() {
        let uri = "zerus://session?machine=11111111-1111-4111-8111-111111111111&host=arch&name=claude%2Fp%2Freview&conversation=c&run=r";
        let body = " \r\n雪 & <tag> \"quoted\" </zerus-message> &amp; &lt; &#65;\n ";
        let frame = envelope("claude/p/review", "arch", uri, body);
        assert!(frame.contains("&lt;/zerus-message&gt;"));
        assert!(frame.contains("&amp;amp; &amp;lt; &amp;#65;"));
        assert_eq!(sender_message(&frame), Some(("claude/p/review".into(), uri.into(), body.into())));
        assert_eq!(sender_message(&envelope("claude/p/review", "arch", uri, "")),
            Some(("claude/p/review".into(), uri.into(), "".into())));
        assert_eq!(frame, envelope("claude/p/review", "arch", uri, body));
    }

    #[test]
    fn sender_xml_refuses_malformed_framing_entities_or_declared_routes() {
        let uri = "zerus://session?machine=11111111-1111-4111-8111-111111111111&host=arch&name=claude%2Fp%2Freview&conversation=c&run=r";
        let frame = envelope("claude/p/review", "arch", uri, "BODY");
        for body in ["raw & value", "<nested/>", "</zerus-message>", "<!DOCTYPE x>", "&#65;", "&#x41;", "&unknown;", "&apos;", "\u{0}", "\u{FFFF}"] {
            assert!(sender_message(&frame.replace("BODY", body)).is_none(), "{body:?}");
        }
        for invalid in [
            frame.replace(" from=", " extra=\"x\" from="),
            frame.replace(" host=\"arch\"", " host=\"other\""),
            frame.replace("from=\"claude/p/review\"", "from=\"claude/p/other\""),
            frame.replace(" reference=", " reference=\"zerus://invalid\" ignored="),
            frame.replace("&amp;host=", "&host="),
            frame.replace("&amp;run=r", "&amp;run=r&amp;run=duplicate"),
            frame.replace("\">\nBODY", "\">BODY"),
            frame.replace("\n</zerus-message>", "</zerus-message>"),
            frame.replace("\n</zerus-message>", "…"),
            format!("{frame}\ntrailing"),
            format!("{frame}{frame}"),
            format!("<!DOCTYPE x>{frame}"),
            envelope("claude/p/review", "arch", "https://example.invalid", "BODY"),
            envelope("claude/p/review", "arch", "zerus://not-a-session", "BODY"),
        ] {
            assert!(sender_message(&invalid).is_none(), "{invalid}");
        }
        assert!(sender_message(&envelope("claude/p/review", "arch", uri, &"x".repeat(64 * 1024))).is_none());
    }
}
