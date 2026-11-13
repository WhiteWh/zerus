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
//! submission counts only once Claude's own transcript records exactly this
//! message: a typed user row or a queue `enqueue` written after the paste with
//! the same lines (zerus-iitw8.4). Both waits share one deadline set once after
//! the paste and never renewed; only the composer is read before Enter, and
//! neither wait holds the state lock (zerus-iitw8.5). Anything else is delivery
//! uncertain, with the composer's bounded text as evidence, and is never
//! replayed.
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

pub(super) fn plain(row: &str) -> String {
    styled_row(row).iter().map(|(ch, _)| ch).collect()
}

/// Claude's unindented full-width rule above and below its composer.
pub(super) fn separator(plain: &str) -> bool {
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

/// One frame of the composer: its text when it shows a settled paste
/// (recognized, not empty, no `Pasting…`), and its text at all. None when
/// the cursor moved during the read.
fn settled(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim) -> Option<(Option<String>, Option<String>)> {
    let (screen, x, y) = frame(terminal, claim)?;
    let text = composer_text(claim.agent, &screen, y).filter(|text| !text.is_empty());
    let shown = text.clone().filter(|_| !pasting(&screen) && !composer_empty_for_model(claim.agent, &screen, x, y, None));
    Some((shown, text))
}

/// Whether the composer still shows exactly `text`, settled: the check made
/// under the state lock immediately before the one Enter (zerus-iitw8.5).
pub(super) fn still(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim, text: &str) -> bool {
    settled(terminal, claim).and_then(|(shown, _)| shown).as_deref() == Some(text)
}

/// Waits until the composer shows the paste settled: recognized, not empty,
/// no `Pasting…`, and the same text on reads at least `STABLE` apart. Ok holds
/// that text; Err the composer's last text when `deadline` passes first. Only
/// the terminal is read, so the caller holds no lock meanwhile.
pub(super) fn settle(
    terminal: &dyn backend::TerminalBackend,
    claim: &backend::InputClaim,
    deadline: Instant,
) -> std::result::Result<String, Option<String>> {
    let mut seen: Option<(String, Instant)> = None;
    let mut last = None;
    loop {
        if let Some((shown, text)) = settled(terminal, claim) {
            last = text.or(last);
            match (shown, &seen) {
                (Some(text), Some((before, at))) if &text == before => {
                    if at.elapsed() >= STABLE {
                        return Ok(text);
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

/// Claude's transcript from its length before the paste: where the proof of
/// this submission must appear. Each read continues after the last complete
/// line (zerus-iitw8.6), at most `TRANSCRIPT_BYTES` after the start in all.
pub(super) struct Transcript {
    path: PathBuf,
    /// Where the next read starts: the start plus every complete line read.
    next: u64,
    /// How many bytes after the start have been read.
    read: u64,
    /// The last line read without its newline yet.
    partial: Vec<u8>,
}

impl Transcript {
    pub(super) fn at(path: PathBuf, start: u64) -> Self {
        Transcript { path, next: start, read: 0, partial: Vec::new() }
    }

    /// The transcript the record names, from its length now. None when the
    /// record names none yet.
    pub(super) fn of(record: &Value) -> Option<Self> {
        let path = PathBuf::from(string(record, "transcript"));
        if !path.is_absolute() {
            return None;
        }
        let start = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        Some(Transcript::at(path, start))
    }

    /// The complete rows appended since the last call, in order.
    fn rows(&mut self) -> Vec<Value> {
        let budget = TRANSCRIPT_BYTES.saturating_sub(self.read);
        if budget == 0 {
            return Vec::new();
        }
        let Ok(mut file) = fs::File::open(&self.path) else {
            return Vec::new();
        };
        let mut bytes = Vec::new();
        let at = self.next + self.partial.len() as u64;
        if file.seek(SeekFrom::Start(at)).is_err() || file.take(budget).read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        self.read += bytes.len() as u64;
        self.partial.extend_from_slice(&bytes);
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return Vec::new();
        };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        self.next += complete.len() as u64;
        complete
            .split(|&b| b == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
            .collect()
    }

    /// The first new row that records `message` as submitted or queued.
    fn proof(&mut self, message: &str, pasted_at: f64) -> Option<Proof> {
        self.rows().iter().find_map(|row| row_proof(row, message, pasted_at))
    }
}

/// What proved the submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Proof {
    /// A new typed user row of the transcript is the message.
    Transcript,
    /// Claude queued it while busy: a new transcript `enqueue` is the message.
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

    /// Whether Claude itself recorded the message, in its transcript or its
    /// queue. An emptied composer alone does not show that it accepted it.
    pub(super) fn recorded(self) -> bool {
        matches!(self, Proof::Transcript | Proof::Queued)
    }
}

/// Waits for Claude's own record of the submission until `deadline`. Err
/// holds the composer's text then, as evidence. `pasted_at` is the wall clock
/// just before the paste: no proving row may be older.
pub(super) fn confirm(
    terminal: &dyn backend::TerminalBackend,
    claim: &backend::InputClaim,
    mut transcript: Option<&mut Transcript>,
    message: &str,
    pasted_at: f64,
    deadline: Instant,
) -> std::result::Result<Proof, Option<String>> {
    loop {
        match transcript.as_deref_mut() {
            Some(transcript) => {
                if let Some(proof) = transcript.proof(message, pasted_at) {
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

/// Whether `row` records exactly `message`, stamped no earlier than the paste
/// (zerus-iitw8.4). Only two rows can be this send:
/// - a user row Claude records for a prompt typed into its composer
///   (`promptSource: typed`, or unmarked in older builds);
/// - a queue `enqueue`, which Claude writes when the prompt is queued.
///
/// A prompt queued earlier and taken now is an earlier send, never this one:
/// its user row is `promptSource: queued`, a `queued_command` attachment or a
/// `remove`/`dequeue` operation, and this send's own enqueue would come first.
fn row_proof(row: &Value, message: &str, pasted_at: f64) -> Option<Proof> {
    let (proof, text) = match row["type"].as_str()? {
        "user" if row["isMeta"] != true && row.get("promptSource").is_none_or(|source| source == "typed") => {
            let text = match &row["message"]["content"] {
                Value::String(text) => text.clone(),
                Value::Array(blocks) => blocks
                    .iter()
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => return None,
            };
            (Proof::Transcript, text)
        }
        "queue-operation" if row["operation"] == "enqueue" => (Proof::Queued, row["content"].as_str()?.to_owned()),
        _ => return None,
    };
    let stamped = chrono::DateTime::parse_from_rfc3339(row["timestamp"].as_str()?).ok()?;
    let stamped = stamped.timestamp_millis() as f64 / 1000.0;
    // Claude may write a row well after it made it (an earlier enqueue lands
    // after this send's start offset), so the stamp, not the position, shows
    // the row is newer than the paste. This send's own row is made at its
    // Enter, after at least the settle's stable time, on the same clock.
    (stamped >= pasted_at && same(message, &text)).then_some(proof)
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

/// Claude Code records a pasted tab as four spaces: inside its
/// `<pasted_content id=...>` wrapper in the transcript and in its
/// UserPromptSubmit hook detail.  Comparison only: fold every run of spaces
/// and tabs inside a line to one space so the proof compares the message the
/// agent recorded, never its whitespace rendering.  Stored and sent text is
/// unchanged.
fn comparison(line: &str) -> String {
    let mut text = String::with_capacity(line.len());
    let mut whitespace = false;
    for ch in line.chars() {
        if ch == ' ' || ch == '\t' {
            whitespace = true;
        } else {
            if whitespace {
                text.push(' ');
                whitespace = false;
            }
            text.push(ch);
        }
    }
    text
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
            comparison(line)
        })
        .filter(|line| !line.is_empty() && !image_path(line))
        .filter(|line| !line.starts_with("<pasted_content") && !line.starts_with("</pasted_content"))
        .collect()
}

/// Whether `recorded` is exactly `message`: the same significant lines in the
/// same order, none missing and none added (zerus-iitw8.4).
pub(super) fn same(message: &str, recorded: &str) -> bool {
    let wanted = significant(message);
    !wanted.is_empty() && wanted == significant(recorded)
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
        assert_eq!(settle(&frames, &claim(), soon(3_000)), Ok(SETTLED.into()));
        assert!(start.elapsed() >= STABLE, "the settled text must hold for {STABLE:?}");
        // Even a composer settled from the first read holds for the whole window.
        let frames = Frames::new(vec![(screen(SETTLED, "paste again to expand"), 50, 2)]);
        let start = Instant::now();
        assert_eq!(settle(&frames, &claim(), soon(3_000)), Ok(SETTLED.into()));
        assert!(start.elapsed() >= STABLE, "entered after {:?}", start.elapsed());
    }

    #[test]
    fn claude_four_space_tabs_are_the_same_message() {
        let message = "13.11 (today)\tSheikh_DM → FarshMaker\t160 GEM";
        let recorded = "13.11 (today)    Sheikh_DM → FarshMaker    160 GEM";
        assert!(same(message, recorded));
        assert!(same(
            message,
            &format!("<pasted_content id=\"x\">\n{recorded}\n</pasted_content>")
        ));
        assert!(same("a\tb", "a  b"));
        // Exactness: another word, an added line, a missing line, another
        // order or a longer line still differs.
        assert!(!same("a\tb", "a c"));
        assert!(!same("a\tb", "a    b\nadded"));
        assert!(!same("a\tb\nmissing", "a    b"));
        assert!(!same("a\tb\nc", "c\na    b"));
        assert!(!same("a\tb", "a    b c"));
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
        assert_eq!(confirm(&frames, &claim(), None, "message", 0.0, soon(2_000)), Ok(Proof::Composer));
        // The owner's case: Enter was swallowed and the placeholders stay.
        let frames = Frames::new(vec![held]);
        assert_eq!(confirm(&frames, &claim(), None, "message", 0.0, soon(300)), Err(Some(SETTLED.into())));
        // Pasting… with an empty composer is not a submission.
        let frames = Frames::new(vec![(screen("", "Pasting…"), 2, 2)]);
        assert_eq!(confirm(&frames, &claim(), None, "message", 0.0, soon(300)), Err(None));
    }

    const MESSAGE: &str = "<zerus-message from=\"codex/x/peer\" host=\"arch\">\nReady.\n- Item 1.\nScreenshots:\n/srv/peer/target/a.png\n\"/srv/peer/target/b c.png\"\n</zerus-message>\n\nInspect the attached image at \"/state/attachments/m/01-row.png\"";

    #[test]
    fn the_transcript_must_record_exactly_the_messages_lines_in_order() {
        // Claude 2.1.322: image lines become [Image #N] tokens, blank lines go.
        let recorded = "[Image #1] [Image #2]<zerus-message from=\"codex/x/peer\" host=\"arch\">\nReady.\n- Item 1.\nScreenshots:\n</zerus-message>\nInspect the attached image at \"/state/attachments/m/01-row.png\"";
        assert!(same(MESSAGE, recorded));
        // Builds that wrap pasted text keep it inside <pasted_content>.
        let wrapped = format!("[Image #153] [Image #154]\n\n<pasted_content id=\"0bc2\">\n{}\n</pasted_content>", recorded.trim_start_matches("[Image #1] [Image #2]"));
        assert!(same(MESSAGE, &wrapped));
        assert!(!same(MESSAGE, &recorded.replace("- Item 1.\n", "")), "a missing line");
        assert!(!same(MESSAGE, &recorded.replace("Ready.\n- Item 1.", "- Item 1.\nReady.")), "reordered");
        assert!(!same(MESSAGE, "another message"));
        assert!(!same("", ""), "an empty message proves nothing");
        // zerus-iitw8.4: another message that also holds every line is not this one.
        assert!(!same("yes", "no\nyes"));
        assert!(!same("Run the tests.", "Fix A.\nRun the tests."));
        assert!(!same("continue", "Please review the PR.\ncontinue"));
        assert!(!same(MESSAGE, &format!("{recorded}\nOne more line.")), "an added line");
        assert!(!same("status?", "status? please"), "a longer line");
    }

    const PASTED: &str = "2026-11-11T20:00:00.000Z";
    /// The wall clock of `PASTED`, as hgs takes it just before the paste.
    fn pasted() -> f64 {
        chrono::DateTime::parse_from_rfc3339(PASTED).unwrap().timestamp_millis() as f64 / 1000.0
    }
    fn typed(content: Value, at: &str) -> Value {
        json!({"type":"user","promptSource":"typed","timestamp":at,"message":{"role":"user","content":content}})
    }
    fn proof(path: &Path, offset: u64, message: &str) -> Option<Proof> {
        Transcript::at(path.to_owned(), offset).proof(message, pasted())
    }

    #[test]
    fn only_typed_rows_or_enqueues_after_the_paste_count_and_only_complete_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let earlier = typed(json!(MESSAGE), "2026-11-11T19:59:00.000Z");
        fs::write(&path, format!("{earlier}\n")).unwrap();
        let offset = fs::metadata(&path).unwrap().len();
        assert_eq!(proof(&path, offset, MESSAGE), None, "an earlier identical row is not this send");
        let tool = typed(json!([{"type":"tool_result","content":MESSAGE}]), PASTED);
        let mut meta = typed(json!(MESSAGE), PASTED);
        meta["isMeta"] = json!(true);
        let user = typed(json!([{"type":"text","text":MESSAGE},{"type":"image","source":{}}]), "2026-11-11T20:00:00.400Z");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{tool}\n{meta}").unwrap();
        write!(file, "{user}").unwrap();
        assert_eq!(proof(&path, offset, MESSAGE), None, "tool results, meta rows and a partial line");
        writeln!(file).unwrap();
        assert_eq!(proof(&path, offset, MESSAGE), Some(Proof::Transcript));
        let queued = dir.path().join("q.jsonl");
        let enqueue = json!({"type":"queue-operation","operation":"enqueue","content":MESSAGE,"timestamp":PASTED});
        fs::write(&queued, format!("{enqueue}\n")).unwrap();
        assert_eq!(proof(&queued, 0, MESSAGE), Some(Proof::Queued));
        for operation in ["remove", "dequeue"] {
            let row = json!({"type":"queue-operation","operation":operation,"content":MESSAGE,"timestamp":PASTED});
            fs::write(&queued, format!("{row}\n")).unwrap();
            assert_eq!(proof(&queued, 0, MESSAGE), None, "{operation}");
        }
    }

    #[test]
    fn an_earlier_queued_prompt_taken_after_the_paste_never_proves_this_send() {
        // zerus-iitw8.4: an earlier "continue" was queued while Claude worked,
        // this send's Enter was lost, and Claude takes the earlier one after
        // the paste. Claude 2.1.322 records that as below (rows of a review run
        // of the private copy against the fixture model).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let before = json!({"type":"queue-operation","operation":"enqueue","content":"continue","timestamp":"2026-11-11T19:59:58.000Z"});
        fs::write(&path, format!("{before}\n")).unwrap();
        let offset = fs::metadata(&path).unwrap().len();
        // Absorbed into a running tool's result.
        let removed = json!({"type":"queue-operation","operation":"remove","content":"continue","commandUuid":"c1","deliveryId":"d1","reason":"absorbed_mid_turn","timestamp":"2026-11-11T20:00:01.000Z"});
        let attached = json!({"type":"attachment","timestamp":"2026-11-11T20:00:01.000Z","attachment":{"type":"queued_command","prompt":"continue","source_uuid":"c1","delivery_id":"d1","origin":{"kind":"human"},"commandMode":"prompt"}});
        // Or taken as the next turn once the reply ends.
        let dequeued = json!({"type":"queue-operation","operation":"dequeue","timestamp":"2026-11-11T20:00:02.000Z"});
        let mut next = typed(json!("continue"), "2026-11-11T20:00:02.000Z");
        next["promptSource"] = json!("queued");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{removed}\n{attached}\n{dequeued}\n{next}").unwrap();
        assert_eq!(proof(&path, offset, "continue"), None);
        // Other prompt sources are not this send's Enter either.
        for source in [json!("suggestion_accepted"), json!("sdk"), json!("system"), json!(null)] {
            let mut row = typed(json!("continue"), "2026-11-11T20:00:02.000Z");
            row["promptSource"] = source.clone();
            fs::write(&path, format!("{row}\n")).unwrap();
            assert_eq!(proof(&path, 0, "continue"), None, "{source}");
        }
        // This send's own enqueue does, after the earlier one is taken.
        let enqueue = json!({"type":"queue-operation","operation":"enqueue","content":"continue","timestamp":"2026-11-11T20:00:00.200Z"});
        fs::write(&path, format!("{removed}\n{attached}\n{enqueue}\n")).unwrap();
        assert_eq!(proof(&path, 0, "continue"), Some(Proof::Queued));
        // An unmarked row from a build before promptSource still counts.
        let mut old = typed(json!("continue"), "2026-11-11T20:00:00.200Z");
        old.as_object_mut().unwrap().remove("promptSource");
        fs::write(&path, format!("{old}\n")).unwrap();
        assert_eq!(proof(&path, 0, "continue"), Some(Proof::Transcript));
    }

    #[test]
    fn a_row_stamped_before_the_paste_or_unstamped_never_proves_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        // Written after the start offset, but made before the paste: Claude
        // 2.1.322 wrote an earlier queued prompt's enqueue 0.45 s before this
        // send's own one, after the send's start offset.
        for at in ["2026-11-11T19:59:59.550Z", "2026-11-11T19:59:59.999Z", "not a time"] {
            let row = typed(json!("continue"), at);
            let enqueue = json!({"type":"queue-operation","operation":"enqueue","content":"continue","timestamp":at});
            fs::write(&path, format!("{row}\n{enqueue}\n")).unwrap();
            assert_eq!(proof(&path, 0, "continue"), None, "{at}");
        }
        let mut unstamped = typed(json!("continue"), PASTED);
        unstamped.as_object_mut().unwrap().remove("timestamp");
        fs::write(&path, format!("{unstamped}\n")).unwrap();
        assert_eq!(proof(&path, 0, "continue"), None);
        // Stamped at the paste or later, it counts.
        fs::write(&path, format!("{}\n", typed(json!("continue"), PASTED))).unwrap();
        assert_eq!(proof(&path, 0, "continue"), Some(Proof::Transcript));
    }

    #[test]
    fn the_transcript_is_read_once_incrementally_and_bounded() {
        // zerus-iitw8.6: each poll reads only what was appended since.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        fs::write(&path, "").unwrap();
        let mut transcript = Transcript::at(path.clone(), 0);
        let other = typed(json!("another prompt"), PASTED).to_string();
        let row = typed(json!("continue"), PASTED).to_string();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{other}").unwrap();
        assert_eq!(transcript.proof("continue", pasted()), None);
        assert_eq!(transcript.next, other.len() as u64 + 1, "the complete line was consumed");
        // A row written in two parts is read whole once its newline lands,
        // also when the read that found its first part ended a complete row.
        let (head, tail) = row.split_at(row.len() / 2);
        write!(file, "{other}\n{head}").unwrap();
        assert_eq!(transcript.proof("continue", pasted()), None);
        assert_eq!(transcript.partial, head.as_bytes());
        writeln!(file, "{tail}").unwrap();
        assert_eq!(transcript.proof("continue", pasted()), Some(Proof::Transcript));
        assert_eq!(transcript.read, (2 * other.len() + row.len() + 3) as u64, "every byte read once");
        // Rows consumed once are not read again.
        assert_eq!(transcript.proof("another prompt", pasted()), None);
        // At most TRANSCRIPT_BYTES after the start, in all the reads together.
        let mut spent = Transcript::at(path.clone(), 0);
        spent.read = TRANSCRIPT_BYTES;
        assert_eq!(spent.proof("another prompt", pasted()), None);
        let mut near = Transcript::at(path, 0);
        near.read = TRANSCRIPT_BYTES - 10;
        assert_eq!(near.proof("another prompt", pasted()), None, "a row beyond the bound is never complete");
        assert_eq!(near.read, TRANSCRIPT_BYTES);
    }

    #[test]
    fn the_typing_marker_lasts_one_window_and_survives_no_clock_step() {
        // zerus-iitw8.5: a crash never leaves a lasting block, and a marker
        // set further ahead than any window is not trusted.
        let at = |until: f64| json!({"input_typing":{"request_id":"r1","until":until}});
        assert_eq!(input_typing(&at(now() + 5.0)), Some("r1"));
        assert_eq!(input_typing(&at(now() - 0.1)), None, "expired");
        assert_eq!(input_typing(&at(now() + TYPING_SECONDS + 60.0)), None, "a clock stepped back");
        assert_eq!(input_typing(&json!({})), None);
        assert_eq!(input_typing(&json!({"input_typing":{"until":now() + 5.0}})), None);
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
