//! Native proof of a Claude terminal submission (zerus-iitw8).
//!
//! Claude Code 2.1.322 turns a pasted line that is only an image path into an
//! `[Image #N]` attachment asynchronously. While it reads and downscales the
//! image its composer stays empty with a `Pasting…` footer, and an Enter
//! pressed then submits an empty prompt, which Claude ignores; the paste lands
//! afterwards and stays in the composer. A fixed delay before Enter therefore
//! cannot tell a submission from a swallowed key.
//!
//! So for Claude the one Enter waits until the composer shows the paste
//! settled (content, no `Pasting…`, the same content on two reads), and the
//! submission counts only once Claude's own transcript records it: a new user
//! row or a queue `enqueue` carrying every line of the message. Both waits
//! share one deadline set once after the paste and never renewed; only the
//! composer is read before Enter. Anything else is delivery uncertain, with
//! the composer's bounded text as evidence, and is never replayed.
use super::*;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Instant;

/// The one window for the settle and the proof, from the end of the paste.
/// It fits inside the desktop's whole-send deadline (30 s, lock wait included).
pub(super) const WINDOW: Duration = Duration::from_secs(8);
/// How long the settled composer must stay the same before Enter.
const STABLE: Duration = Duration::from_millis(150);
/// How often the composer or transcript is read again.
const POLL: Duration = Duration::from_millis(50);
/// The most composer text a receipt keeps.
pub(crate) const RESIDUE_CHARS: usize = 2_000;
/// The most transcript bytes read after the send's starting offset.
const TRANSCRIPT_BYTES: u64 = 16 * 1024 * 1024;

/// The agents whose submissions need this proof.
pub(super) fn applies(agent: &str) -> bool {
    agent == "claude"
}

fn plain(row: &str) -> String {
    styled_row(row).iter().map(|(ch, _)| ch).collect()
}

/// Claude's unindented full-width rule above and below its composer.
fn separator(plain: &str) -> bool {
    !plain.starts_with(char::is_whitespace)
        && plain.chars().count() >= 12
        && plain.chars().all(|ch| "─━╌".contains(ch))
}

/// Claude's footer while it still processes a paste.
pub(super) fn pasting(screen: &str) -> bool {
    let rows: Vec<String> = screen.lines().map(plain).collect();
    let Some(last_rule) = rows.iter().rposition(|row| separator(row)) else {
        return false;
    };
    rows[last_rule + 1..].iter().any(|row| row.trim_start().starts_with("Pasting…"))
}

/// The text of Claude's composer around `cursor_y`: from its `❯` row to the
/// rule below, without the prompt glyph or the continuation indent, bounded.
/// None when no composer is recognized there.
pub(super) fn composer_text(agent: &str, screen: &str, cursor_y: usize) -> Option<String> {
    let rows: Vec<String> = screen.lines().map(plain).collect();
    let text = match agent {
        "claude" => {
            let mut start = None;
            for index in (0..=cursor_y.min(rows.len().checked_sub(1)?)).rev() {
                if separator(&rows[index]) {
                    break;
                }
                let mut chars = rows[index].chars();
                if matches!(chars.next(), Some('❯' | '>')) && chars.next().is_none_or(char::is_whitespace) {
                    start = Some(index);
                    break;
                }
            }
            let start = start?;
            // Claude's rule below the composer, or a blank row in simpler layouts.
            let end = (start + 1..rows.len())
                .find(|&index| separator(&rows[index]) || rows[index].trim().is_empty())
                .unwrap_or(rows.len());
            rows[start..end]
                .iter()
                .enumerate()
                .map(|(n, row)| {
                    let row: String = if n == 0 { row.chars().skip(2).collect() } else { row.strip_prefix("  ").unwrap_or(row).to_owned() };
                    row.trim_end().to_owned()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => {
            // Any other agent: the rows around the cursor, as evidence only.
            let from = cursor_y.saturating_sub(2);
            rows.get(from..(cursor_y + 3).min(rows.len()))?
                .iter()
                .map(|row| row.trim_end())
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    let text = text.trim().to_owned();
    Some(text.chars().take(RESIDUE_CHARS).collect())
}

/// One composer frame: its styled screen and the cursor, read with the same
/// cursor before and after the capture.
pub(crate) fn frame(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim) -> Option<(String, usize, usize)> {
    let before = terminal.input_state(claim).ok()?;
    let screen = String::from_utf8_lossy(&terminal.capture(claim.pane, backend::Capture::Styled).ok()?).into_owned();
    let after = terminal.input_state(claim).ok()?;
    if before.cursor_x != after.cursor_x || before.cursor_y != after.cursor_y {
        return None;
    }
    Some((screen, after.cursor_x.parse().ok()?, after.cursor_y.parse().ok()?))
}

/// The composer's text now, as evidence (bounded), or None.
pub(super) fn residue(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim) -> Option<String> {
    let (screen, _, y) = frame(terminal, claim)?;
    composer_text(claim.agent, &screen, y).filter(|text| !text.is_empty())
}

/// Waits until the composer shows the paste settled: recognized, not empty,
/// no `Pasting…`, and the same text on reads at least `STABLE` apart. Err
/// holds the composer's last text when `deadline` passes first.
pub(super) fn settle(
    terminal: &dyn backend::TerminalBackend,
    claim: &backend::InputClaim,
    deadline: Instant,
) -> std::result::Result<(), Option<String>> {
    let mut seen: Option<(String, Instant)> = None;
    let mut last = None;
    loop {
        if let Some((screen, x, y)) = frame(terminal, claim) {
            let text = composer_text(claim.agent, &screen, y).filter(|text| !text.is_empty());
            last = text.clone().or(last);
            let shown = text.filter(|_| {
                !pasting(&screen) && !composer_empty_for_model(claim.agent, &screen, x, y, None)
            });
            match (shown, &seen) {
                (Some(text), Some((before, at))) if &text == before => {
                    if at.elapsed() >= STABLE {
                        return Ok(());
                    }
                }
                (Some(text), _) => seen = Some((text, Instant::now())),
                (None, _) => seen = None,
            }
        }
        if Instant::now() >= deadline {
            return Err(last);
        }
        std::thread::sleep(POLL);
    }
}

/// Claude's transcript and its length before the paste: where the proof of
/// this submission must appear. None when the record names none yet.
pub(super) fn transcript(record: &Value) -> Option<(PathBuf, u64)> {
    let path = PathBuf::from(string(record, "transcript"));
    if !path.is_absolute() {
        return None;
    }
    Some((path.clone(), fs::metadata(&path).map(|m| m.len()).unwrap_or(0)))
}

/// What proved the submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Proof {
    /// A new user row of the transcript carries the message.
    Transcript,
    /// Claude queued it while busy: a transcript `enqueue` carries it.
    Queued,
    /// No transcript was known yet (a new conversation's first message): the
    /// settled composer emptied after the Enter.
    Composer,
}

impl Proof {
    pub(super) fn name(self) -> &'static str {
        match self {
            Proof::Transcript => "transcript",
            Proof::Queued => "queue",
            Proof::Composer => "composer",
        }
    }
}

/// Waits for Claude's own record of the submission until `deadline`. Err
/// holds the composer's text then, as evidence.
pub(super) fn confirm(
    terminal: &dyn backend::TerminalBackend,
    claim: &backend::InputClaim,
    transcript: Option<&(PathBuf, u64)>,
    message: &str,
    deadline: Instant,
) -> std::result::Result<Proof, Option<String>> {
    loop {
        match transcript {
            Some((path, offset)) => {
                if let Some(proof) = transcript_proof(path, *offset, message) {
                    return Ok(proof);
                }
            }
            None => {
                if let Some((screen, x, y)) = frame(terminal, claim) {
                    if !pasting(&screen) && composer_empty_for_model(claim.agent, &screen, x, y, None) {
                        return Ok(Proof::Composer);
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(residue(terminal, claim));
        }
        std::thread::sleep(POLL);
    }
}

/// The first row appended after `offset` that records `message` as submitted
/// or queued. Only complete lines are read, at most `TRANSCRIPT_BYTES`.
pub(super) fn transcript_proof(path: &Path, offset: u64, message: &str) -> Option<Proof> {
    let mut file = fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = Vec::new();
    file.take(TRANSCRIPT_BYTES).read_to_end(&mut bytes).ok()?;
    let complete = bytes.iter().rposition(|&b| b == b'\n').map_or(0, |end| end + 1);
    for line in bytes[..complete].split(|&b| b == b'\n') {
        let Ok(row) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if let Some(proof) = row_proof(&row, message) {
            return Some(proof);
        }
    }
    None
}

fn row_proof(row: &Value, message: &str) -> Option<Proof> {
    let text = match row["type"].as_str()? {
        "user" if row["isMeta"] != true => match &row["message"]["content"] {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => return None,
        },
        "queue-operation" if row["operation"] == "enqueue" => row["content"].as_str()?.to_owned(),
        "attachment" if row["attachment"]["type"] == "queued_command" => row["attachment"]["prompt"].as_str()?.to_owned(),
        _ => return None,
    };
    let proof = if row["type"] == "user" { Proof::Transcript } else { Proof::Queued };
    carries(message, &text).then_some(proof)
}

/// A line Claude may turn into an image attachment: only a path (quoted or
/// not) to an image file.
fn image_path(line: &str) -> bool {
    let line = line.trim();
    let path = line.strip_prefix('"').and_then(|l| l.strip_suffix('"')).unwrap_or(line);
    let lower = path.to_ascii_lowercase();
    (path.starts_with('/') || path.starts_with('~') || path.get(1..3) == Some(":\\"))
        && (path == line && !path.contains(char::is_whitespace) || path != line)
        && [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tif", ".tiff", ".heic"].iter().any(|ext| lower.ends_with(ext))
}

/// The lines that must reach the transcript, in order: the message without
/// blank lines, image-path lines Claude attaches instead, Claude's own
/// `[Image #N]` tokens and its `<pasted_content>` wrapper.
fn significant(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| {
            let mut line = line.trim();
            while let Some(rest) = line.strip_prefix("[Image #").and_then(|rest| {
                let digits = rest.find(|ch: char| !ch.is_ascii_digit())?;
                (digits > 0).then(|| &rest[digits..]).and_then(|rest| rest.strip_prefix(']'))
            }) {
                line = rest.trim_start();
            }
            line.to_owned()
        })
        .filter(|line| !line.is_empty() && !image_path(line))
        .filter(|line| !line.starts_with("<pasted_content") && !line.starts_with("</pasted_content"))
        .collect()
}

/// Whether `recorded` carries every significant line of `message`, in order.
pub(super) fn carries(message: &str, recorded: &str) -> bool {
    let wanted = significant(message);
    let mut have = significant(recorded).into_iter();
    !wanted.is_empty() && wanted.iter().all(|line| have.any(|other| &other == line))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn rule() -> String {
        "─".repeat(40)
    }
    /// A Claude screen whose composer holds `composer` and whose footer is `footer`.
    fn screen(composer: &str, footer: &str) -> String {
        format!("● Fixture complete.\n{}\n❯ {composer}\n{}\n  {footer}\n", rule(), rule())
    }
    const SETTLED: &str = "[Image #1] [Image #2][Pasted text #3 +35 lines]";

    /// One captured screen with the cursor column and row.
    type Frame = (String, usize, usize);
    /// Frames shown in order, one per capture; the last one stays.
    struct Frames(Mutex<(Vec<Frame>, usize)>);
    impl Frames {
        fn new(frames: Vec<Frame>) -> Self {
            Frames(Mutex::new((frames, 0)))
        }
        fn current(&self) -> Frame {
            let state = self.0.lock().unwrap();
            state.0[state.1.min(state.0.len() - 1)].clone()
        }
    }
    impl backend::TerminalBackend for Frames {
        fn input_state(&self, _claim: &backend::InputClaim) -> Result<backend::InputState> {
            let (_, x, y) = self.current();
            Ok(backend::InputState { cursor_x: x.to_string(), cursor_y: y.to_string() })
        }
        fn capture(&self, _pane: &str, _mode: backend::Capture) -> Result<Vec<u8>> {
            let mut state = self.0.lock().unwrap();
            let shown = state.0[state.1.min(state.0.len() - 1)].0.clone();
            state.1 += 1;
            Ok(shown.into_bytes())
        }
    }
    fn claim() -> backend::InputClaim<'static> {
        backend::InputClaim { pane: "%1", name: "claude/p/s", run_id: "r", agent: "claude", pid: None }
    }
    fn soon(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn pasting_is_only_claudes_footer_below_the_composer() {
        assert!(pasting(&screen("", "Pasting…")));
        assert!(!pasting(&screen(SETTLED, "paste again to expand")));
        // The words in the conversation above the composer are not the footer.
        assert!(!pasting(&format!("Pasting…\n{}", screen("", "⏵⏵ bypass permissions on"))));
    }

    #[test]
    fn the_composer_text_is_read_between_its_rules_bounded() {
        assert_eq!(composer_text("claude", &screen(SETTLED, "paste again to expand"), 2).as_deref(), Some(SETTLED));
        let multi = format!("{}\n❯ first line\n  second line\n{}\n", rule(), rule());
        assert_eq!(composer_text("claude", &multi, 2).as_deref(), Some("first line\nsecond line"));
        assert_eq!(composer_text("claude", &screen("", "x"), 2).as_deref(), Some(""));
        assert_eq!(composer_text("claude", "no prompt here\n", 0), None);
        // A layout without rules ends at the first blank row.
        assert_eq!(composer_text("claude", "❯ echoed\nsecond\n\nolder output\n", 1).as_deref(), Some("echoed\nsecond"));
        let long = screen(&"x".repeat(RESIDUE_CHARS + 50), "x");
        assert_eq!(composer_text("claude", &long, 2).unwrap().chars().count(), RESIDUE_CHARS);
    }

    #[test]
    fn enter_waits_until_the_pasted_images_settle_in_the_composer() {
        // zerus-iitw8: empty with Pasting… first, then the placeholders.
        let pasting_frame = (screen("", "Pasting…"), 2, 2);
        let settled = (screen(SETTLED, "paste again to expand"), 50, 2);
        let frames = Frames::new(vec![pasting_frame.clone(), pasting_frame.clone(), pasting_frame, settled]);
        let start = Instant::now();
        assert_eq!(settle(&frames, &claim(), soon(3_000)), Ok(()));
        assert!(start.elapsed() >= STABLE, "the settled text must hold for {STABLE:?}");
        // Even a composer settled from the first read holds for the whole window.
        let frames = Frames::new(vec![(screen(SETTLED, "paste again to expand"), 50, 2)]);
        let start = Instant::now();
        assert_eq!(settle(&frames, &claim(), soon(3_000)), Ok(()));
        assert!(start.elapsed() >= STABLE, "entered after {:?}", start.elapsed());
    }

    #[test]
    fn a_composer_that_never_settles_is_never_entered() {
        let frames = Frames::new(vec![(screen("", "Pasting…"), 2, 2)]);
        assert_eq!(settle(&frames, &claim(), soon(300)), Err(None));
        // An empty composer is not a settled paste either.
        let frames = Frames::new(vec![(screen("", "⏵⏵ bypass permissions on"), 2, 2)]);
        assert_eq!(settle(&frames, &claim(), soon(300)), Err(None));
        // Text that keeps changing never settles; the last text is the evidence.
        let changing: Vec<_> = (0..40).map(|n| (screen(&format!("draft {n}"), "x"), 10, 2)).collect();
        assert!(matches!(settle(&Frames::new(changing), &claim(), soon(300)), Err(Some(text)) if text.starts_with("draft")));
    }

    #[test]
    fn without_a_transcript_only_an_emptied_composer_confirms() {
        let empty = (screen("", "⏵⏵ bypass permissions on"), 2, 2);
        let held = (screen(SETTLED, "paste again to expand"), 50, 2);
        let frames = Frames::new(vec![held.clone(), empty]);
        assert_eq!(confirm(&frames, &claim(), None, "message", soon(2_000)), Ok(Proof::Composer));
        // The owner's case: Enter was swallowed and the placeholders stay.
        let frames = Frames::new(vec![held]);
        assert_eq!(confirm(&frames, &claim(), None, "message", soon(300)), Err(Some(SETTLED.into())));
        // Pasting… with an empty composer is not a submission.
        let frames = Frames::new(vec![(screen("", "Pasting…"), 2, 2)]);
        assert_eq!(confirm(&frames, &claim(), None, "message", soon(300)), Err(None));
    }

    const MESSAGE: &str = "<zerus-message from=\"codex/x/peer\" host=\"arch\">\nReady.\n- Item 1.\nScreenshots:\n/srv/peer/target/a.png\n\"/srv/peer/target/b c.png\"\n</zerus-message>\n\nInspect the attached image at \"/state/attachments/m/01-row.png\"";

    #[test]
    fn the_transcript_must_carry_every_line_of_the_message_in_order() {
        // Claude 2.1.322: image lines become [Image #N] tokens, blank lines go.
        let recorded = "[Image #1] [Image #2]<zerus-message from=\"codex/x/peer\" host=\"arch\">\nReady.\n- Item 1.\nScreenshots:\n</zerus-message>\nInspect the attached image at \"/state/attachments/m/01-row.png\"";
        assert!(carries(MESSAGE, recorded));
        // Builds that wrap pasted text keep it inside <pasted_content>.
        let wrapped = format!("[Image #153] [Image #154]\n\n<pasted_content id=\"0bc2\">\n{}\n</pasted_content>", recorded.trim_start_matches("[Image #1] [Image #2]"));
        assert!(carries(MESSAGE, &wrapped));
        assert!(!carries(MESSAGE, &recorded.replace("- Item 1.\n", "")), "a missing line");
        assert!(!carries(MESSAGE, &recorded.replace("Ready.\n- Item 1.", "- Item 1.\nReady.")), "reordered");
        assert!(!carries(MESSAGE, "another message"));
        assert!(!carries("", ""), "an empty message proves nothing");
    }

    #[test]
    fn only_rows_after_the_send_count_and_only_complete_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let earlier = json!({"type":"user","message":{"content":MESSAGE}});
        fs::write(&path, format!("{earlier}\n")).unwrap();
        let offset = fs::metadata(&path).unwrap().len();
        assert_eq!(transcript_proof(&path, offset, MESSAGE), None, "an earlier identical row is not this send");
        let tool = json!({"type":"user","message":{"content":[{"type":"tool_result","content":MESSAGE}]}});
        let meta = json!({"type":"user","isMeta":true,"message":{"content":MESSAGE}});
        let user = json!({"type":"user","message":{"content":[{"type":"text","text":MESSAGE},{"type":"image","source":{}}]}});
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{tool}\n{meta}").unwrap();
        write!(file, "{user}").unwrap();
        assert_eq!(transcript_proof(&path, offset, MESSAGE), None, "tool results, meta rows and a partial line");
        writeln!(file).unwrap();
        assert_eq!(transcript_proof(&path, offset, MESSAGE), Some(Proof::Transcript));
        let queued = dir.path().join("q.jsonl");
        fs::write(&queued, format!("{}\n", json!({"type":"queue-operation","operation":"enqueue","content":MESSAGE}))).unwrap();
        assert_eq!(transcript_proof(&queued, 0, MESSAGE), Some(Proof::Queued));
        fs::write(&queued, format!("{}\n", json!({"type":"queue-operation","operation":"remove","content":MESSAGE}))).unwrap();
        assert_eq!(transcript_proof(&queued, 0, MESSAGE), None);
    }

    #[test]
    fn a_busy_agents_later_events_never_confirm_typed_input() {
        let record = json!({"input_pending_at":100.0,"input_seen_at":50.0,"last_event_at":200.0});
        assert!(input_unconfirmed(&record), "tool events after the send are not acceptance");
        assert!(!input_unconfirmed(&json!({"input_pending_at":100.0,"input_seen_at":101.0,"last_event_at":101.0})));
        // A record from before the rule keeps the event rule it was written under.
        assert!(!input_unconfirmed(&json!({"input_pending_at":100.0,"last_event_at":200.0})));
        assert!(input_unconfirmed(&json!({"input_pending_at":100.0,"last_event_at":100.0})));
        assert!(!input_unconfirmed(&json!({"last_event_at":0.0})));
    }
}
