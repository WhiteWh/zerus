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
//!
//! A pasted message Claude keeps inline (at most three lines and about 800
//! characters; longer ones become a `[Pasted text #N]` placeholder) can be
//! taller than the composer, whose height follows the pane's (seven rows at
//! 80x24, tmux's size for a session no client has attached to). Claude then
//! shows only its last rows, under a `─── ↑ N more ───` bar, and its prompt
//! row is out of view (zerus-33caz). Such a composer settles only when its
//! shown rows, the rows the bar counts and the cursor are exactly Claude's
//! own layout of this paste at the pane's width.
//!
//! The rows out of view above that bar cannot be read, so text typed before
//! the paste must not be there: the send gate reads the composer empty under
//! the state lock, and a person can still type into the terminal after it.
//! The composer is therefore read empty once more right before the paste
//! (zerus-7cq23); only keys that reach Claude after that read, or that it has
//! read but not yet drawn, remain unseen. A composer with any row out of view
//! below, or whose closing rule is not on the screen, never settles.
//!
//! The read right before the paste keeps its own frame's text as the evidence
//! of a refusal, and a frame that moves while it is read is waited for
//! without the state lock, then read again under it (zerus-oz7cz). A pane too
//! short to show the closing rule (seven rows or fewer at 2.1.327) is refused
//! by the send gate before anything is published, and by this read when the
//! pane shrinks after the gate; only a shrink during the settle leaves the
//! paste in the composer, as delivery uncertain.
use super::*;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::Instant;

/// The one window for the settle and the proof, from the end of the paste.
/// It fits inside the desktop's whole-send deadline (30 s, lock wait included).
pub(in crate::state) const WINDOW: Duration = Duration::from_secs(8);
/// How long the settled composer must stay the same before Enter.
const STABLE: Duration = Duration::from_millis(150);
/// How often the composer or transcript is read again.
const POLL: Duration = Duration::from_millis(50);
/// The most composer text a receipt keeps.
pub(crate) const RESIDUE_CHARS: usize = 2_000;
/// The most transcript bytes read after the send's starting offset.
const TRANSCRIPT_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
mod source_tests;

#[cfg(test)]
std::thread_local! {
    static AFTER_TRANSCRIPT_READ: std::cell::Cell<Option<fn(&std::path::Path)>> = const { std::cell::Cell::new(None) };
}

/// The agents whose submissions need this proof.
pub(in crate::state) fn applies(agent: &str) -> bool {
    agent == "claude"
}

pub(in crate::state) fn plain(row: &str) -> String {
    styled_row(row).iter().map(|(ch, _)| ch).collect()
}

/// Claude's unindented full-width rule above and below its composer.
pub(in crate::state) fn separator(plain: &str) -> bool {
    !plain.starts_with(char::is_whitespace)
        && plain.chars().count() >= 12
        && plain.chars().all(|ch| "─━╌".contains(ch))
}

/// Claude's footer while it still processes a paste.
pub(in crate::state) fn pasting(screen: &str) -> bool {
    let rows: Vec<String> = screen.lines().map(plain).collect();
    let Some(last_rule) = rows.iter().rposition(|row| separator(row)) else {
        return false;
    };
    rows[last_rule + 1..].iter().any(|row| row.trim_start().starts_with("Pasting…"))
}

/// Claude's bar over composer rows out of view (Claude Code 2.1.326): an
/// unindented `─── ↑ N more ───…` above the rows it shows, or `─── ↓ N more
/// ───…` below them, as wide as the pane. Whether the rows are above, and N.
fn more_bar(plain: &str) -> Option<(bool, usize)> {
    let rest = plain.strip_prefix("─── ")?;
    let (above, rest) = match rest.strip_prefix("↑ ") {
        Some(rest) => (true, rest),
        None => (false, rest.strip_prefix("↓ ")?),
    };
    let (count, rule) = rest.split_once(" more ")?;
    if count.is_empty() || !count.bytes().all(|b| b.is_ascii_digit()) || rule.is_empty() || !rule.chars().all(|ch| ch == '─') {
        return None;
    }
    Some((above, count.parse().ok().filter(|&count: &usize| count > 0)?))
}

/// What one frame shows of Claude's composer.
enum View {
    /// From its `❯` prompt row; `below` rows are out of view under it, and
    /// None when no closing rule or bar shows whether any are (zerus-7cq23).
    Whole { text: String, below: Option<usize> },
    /// A composer taller than its view, under its `↑ N more` bar.
    Scrolled(Scrolled),
}

/// The rows a scrolled composer shows (zerus-33caz).
struct Scrolled {
    /// Rows out of view above the shown ones, its prompt row first: never read.
    above: usize,
    /// Rows out of view below the cursor's (`↓ N more`), 0 under Claude's rule.
    below: usize,
    /// The shown rows without the two-cell indent, trimmed at the end.
    rows: Vec<String>,
    /// The screen row of the first shown row.
    first: usize,
    /// The bar's and the closing row's widths: the pane's columns.
    width: usize,
    closing: usize,
}

impl Scrolled {
    /// Whether these rows can only be exactly `pasted`: Claude's own rows for
    /// that text at this width, `above` of them out of view and every other
    /// one shown, nothing out of view below, and the cursor right after its
    /// last character. Rows out of view cannot be read: text typed into the
    /// composer before the paste is excluded by the empty composer read right
    /// before the paste (`before_paste`), as for a composer in full view, and by this
    /// count wherever it adds a row; text typed later shows at the cursor.
    fn holds(&self, pasted: &str, cursor_x: usize, cursor_y: usize) -> bool {
        if self.below != 0 || self.rows.is_empty() || self.width != self.closing || self.width <= GUTTER * 2 {
            return false;
        }
        let Some(layout) = claude_rows(pasted, self.width - GUTTER * 2) else {
            return false;
        };
        let Some(last) = layout.last() else {
            return false;
        };
        layout.len() == self.above + self.rows.len()
            && layout[self.above..].iter().zip(&self.rows).all(|(row, shown)| row.text.trim_end() == shown)
            && cursor_y == self.first + self.rows.len() - 1
            && cursor_x == GUTTER + last.cells
    }
}

/// The cells left of Claude's composer text (`❯ ` or the continuation indent),
/// and the same margin at its right.
const GUTTER: usize = 2;

/// Claude's composer around `cursor_y`, from plain rows.
fn claude_view(rows: &[String], cursor_y: usize) -> Option<View> {
    let mut found = None;
    for index in (0..=cursor_y.min(rows.len().checked_sub(1)?)).rev() {
        if separator(&rows[index]) {
            break;
        }
        if let Some((above, count)) = more_bar(&rows[index]) {
            // A `↓` bar above the cursor closes a composer the cursor is not in.
            found = above.then_some((index, Some(count)));
            break;
        }
        let mut chars = rows[index].chars();
        if matches!(chars.next(), Some('❯' | '>')) && chars.next().is_none_or(char::is_whitespace) {
            found = Some((index, None));
            break;
        }
    }
    let (start, above) = found?;
    let closes = |index: &usize| separator(&rows[*index]) || more_bar(&rows[*index]).is_some();
    let below = |index: usize| rows.get(index).and_then(|row| more_bar(row)).filter(|(above, _)| !above).map_or(0, |(_, count)| count);
    let Some(above) = above else {
        // Claude's rule or `↓` bar closes its composer: every row up to it is
        // the composer's, blank ones (its empty lines) included (zerus-7cq23).
        // A layout without either never settles, and its text is evidence
        // only: it ends at its first row that is blank or not indented as
        // Claude's continuation rows are, so rows of the conversation under
        // the prompt stay out of the receipt (zerus-szz00).
        let closing = (start + 1..rows.len()).find(closes);
        let end = closing.unwrap_or_else(|| {
            (start + 1..rows.len())
                .find(|index| rows[*index].trim().is_empty() || !rows[*index].starts_with("  "))
                .unwrap_or(rows.len())
        });
        let text = rows[start..end]
            .iter()
            .enumerate()
            .map(|(n, row)| {
                let row: String = if n == 0 { row.chars().skip(2).collect() } else { row.strip_prefix("  ").unwrap_or(row).to_owned() };
                row.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n");
        // An `↑` bar under the prompt row is no layout of Claude's composer.
        let below = closing.and_then(|end| match more_bar(&rows[end]) {
            Some((true, _)) => None,
            _ => Some(below(end)),
        });
        return Some(View::Whole { text: text.trim().to_owned(), below });
    };
    // Every row up to Claude's closing rule or bar is a composer row.
    let end = (start + 1..rows.len()).find(closes)?;
    if more_bar(&rows[end]).is_some_and(|(above, _)| above) {
        return None;
    }
    let shown = rows[start + 1..end]
        .iter()
        .map(|row| (row.is_empty() || row.starts_with("  ")).then(|| row.get(GUTTER..).unwrap_or("").trim_end().to_owned()))
        .collect::<Option<Vec<_>>>()?;
    Some(View::Scrolled(Scrolled {
        above,
        below: below(end),
        rows: shown,
        first: start + 1,
        width: rows[start].chars().count(),
        closing: rows[end].chars().count(),
    }))
}

/// One row of Claude's composer: its text and its cells.
struct Row {
    text: String,
    cells: usize,
}

/// Claude Code's composer rows for `text`, `columns` cells wide (2.1.326, its
/// pane's columns less four), as its own screens show them
/// (tests/fixtures/claude-composer-layout-2.1.326.json): each line wrapped as
/// wrap-ansi does with `hard: true, trim: false`, then each wrapped row shown
/// without its leading spaces. None for text this does not measure (a control,
/// zero-width or combining character): such a composer is never proved.
fn claude_rows(text: &str, columns: usize) -> Option<Vec<Row>> {
    if columns == 0 {
        return None;
    }
    let mut shown = Vec::new();
    for line in text.split('\n') {
        for (index, (text, cells)) in wrap_line(line, columns)?.into_iter().enumerate() {
            let kept = if index == 0 { text.as_str() } else { text.trim_start_matches(' ') };
            shown.push(Row { cells: cells - (text.len() - kept.len()), text: kept.to_owned() });
        }
    }
    Some(shown)
}

/// The cells of one character, for those Claude and tmux measure alike.
fn cells(ch: char) -> Option<usize> {
    use unicode_width::UnicodeWidthChar;
    ch.width().filter(|cells| matches!(cells, 1 | 2))
}

/// One line as wrap-ansi 9 wraps it with `hard: true, trim: false`: its rows
/// and their cells, every space kept.
fn wrap_line(line: &str, columns: usize) -> Option<Vec<(String, usize)>> {
    let mut rows = vec![(String::new(), 0)];
    for (index, word) in line.split(' ').enumerate() {
        let length = word.chars().map(cells).sum::<Option<usize>>()?;
        let mut row_length = rows.last()?.1;
        if index != 0 {
            if row_length >= columns {
                rows.push((String::new(), 0));
                row_length = 0;
            }
            let row = rows.last_mut()?;
            row.0.push(' ');
            row.1 += 1;
            row_length += 1;
        }
        if length > columns {
            // A word longer than a row starts on the next row only when that
            // saves a break; it then fills each row to the last cell.
            let remaining = columns - row_length;
            if (length - 1) / columns < 1 + (length - remaining - 1) / columns {
                rows.push((String::new(), 0));
            }
            let mut visible = rows.last()?.1;
            let count = word.chars().count();
            for (n, ch) in word.chars().enumerate() {
                let cell = cells(ch)?;
                if visible + cell <= columns {
                    let row = rows.last_mut()?;
                    row.0.push(ch);
                    row.1 += cell;
                } else {
                    rows.push((ch.to_string(), cell));
                    visible = 0;
                }
                visible += cell;
                if visible == columns && n + 1 < count {
                    rows.push((String::new(), 0));
                    visible = 0;
                }
            }
            continue;
        }
        if row_length + length > columns && row_length > 0 && length > 0 {
            rows.push((String::new(), 0));
        }
        let row = rows.last_mut()?;
        row.0.push_str(word);
        row.1 += length;
    }
    Some(rows)
}

/// The text of Claude's composer around `cursor_y`: from its `❯` row to the
/// rule below, without the prompt glyph or the continuation indent, bounded.
/// A composer taller than its view gives `↑ N more` and the rows it shows (and
/// `↓ N more` for rows out of view below). None when no composer is
/// recognized there.
pub(in crate::state) fn composer_text(agent: &str, screen: &str, cursor_y: usize) -> Option<String> {
    Some(composer_full(agent, screen, cursor_y)?.chars().take(RESIDUE_CHARS).collect())
}

// The Enter fence compares all captured composer text; only evidence is shortened.
fn composer_full(agent: &str, screen: &str, cursor_y: usize) -> Option<String> {
    let rows: Vec<String> = screen.lines().map(plain).collect();
    let text = match agent {
        "claude" => match claude_view(&rows, cursor_y)? {
            View::Whole { text, below: Some(0) | None } => text,
            View::Whole { text, below: Some(below) } => format!("{text}\n↓ {below} more"),
            View::Scrolled(view) => {
                let below = if view.below > 0 { format!("\n↓ {} more", view.below) } else { String::new() };
                format!("↑ {} more\n{}{below}", view.above, view.rows.join("\n"))
            }
        },
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
    Some(text)
}

/// The pane's cursor for a frame. WhiteWh fork (owner, 2026-11-13): the
/// pane's own cursor fields, one cheap terminal read. `input_state` gathers
/// the console's full evidence (its processes and their command lines, input
/// modes) through the pane's supervisor, which under a build's load takes so
/// long that an eight-second window read no composer at all and a delivered
/// paste stayed unsent. The input judge still runs once before the Enter
/// (`checked_terminal`); a backend without pane fields keeps `input_state`.
fn cursor(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim) -> Option<(usize, usize)> {
    use backend::PaneField::{CursorX, CursorY};
    if let Ok(info) = terminal.pane_info(backend::Target::Pane(claim.pane), &[CursorX, CursorY]) {
        if let Some((Ok(x), Ok(y))) = info.pair().map(|(x, y)| (x.parse(), y.parse())) {
            return Some((x, y));
        }
    }
    let state = terminal.input_state(claim).ok()?;
    Some((state.cursor_x.parse().ok()?, state.cursor_y.parse().ok()?))
}

/// One composer frame: its styled screen and the cursor, read with the same
/// cursor before and after the capture.
pub(crate) fn frame(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim) -> Option<(String, usize, usize)> {
    let before = cursor(terminal, claim)?;
    let screen = String::from_utf8_lossy(&terminal.capture(claim.pane, backend::Capture::Styled).ok()?).into_owned();
    let after = cursor(terminal, claim)?;
    if before != after {
        return None;
    }
    Some((screen, after.0, after.1))
}

/// The composer's text now, as evidence (bounded), or None.
pub(in crate::state) fn residue(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim) -> Option<String> {
    let (screen, _, y) = frame(terminal, claim)?;
    composer_text(claim.agent, &screen, y).filter(|text| !text.is_empty())
}

/// The send gate's refusal of a Claude composer the pane cuts off (zerus-oz7cz).
pub(in crate::state) const SHORT: &str =
    "Claude's pane is too short to show its whole composer; nothing was sent; make the pane taller and send again";

/// Whether the pane's bottom edge cuts off Claude's composer around
/// `cursor_y`: no closing rule or bar under its prompt row on the screen, and
/// no blank row from the cursor's row down to the screen's last row, so the
/// composer runs into the pane's edge. Claude Code 2.1.327 draws its prompt
/// row on the last row of a pane seven rows tall or shorter (eight still shows
/// the rule). Rows of the composer under the prompt row are then out of view,
/// so a paste there never settles (zerus-oz7cz). A prompt with blank rows
/// under it and no rule is no such edge: it is not refused as too short, and
/// still never settles (zerus-7cq23).
pub(in crate::state) fn cut(agent: &str, screen: &str, cursor_y: usize) -> bool {
    let rows: Vec<String> = screen.lines().map(plain).collect();
    agent == "claude"
        && matches!(claude_view(&rows, cursor_y), Some(View::Whole { below: None, .. }))
        && rows.iter().skip(cursor_y + 1).all(|row| !row.trim().is_empty())
}

/// How long the wait before the paste, unlocked, gives a frame to hold still.
const BEFORE_PASTE: Duration = Duration::from_millis(250);

/// What one read of Claude's composer right before the paste found
/// (zerus-7cq23).
pub(in crate::state) enum Before {
    /// An empty prompt with its closing rule on the screen.
    Empty,
    /// Anything else: why, and the composer's bounded text in that same frame
    /// (zerus-oz7cz), None when it shows none.
    Refused(&'static str, Option<String>),
    /// The cursor moved while the frame was read, or the terminal was not read.
    Moving,
}

impl Before {
    /// The refusal and its evidence, when this read allows no paste.
    pub(in crate::state) fn refused(self) -> Option<(&'static str, Option<String>)> {
        match self {
            Before::Empty => None,
            Before::Refused(reason, composer) => Some((reason, composer)),
            Before::Moving => Some(("Claude's composer did not hold still right before the paste; nothing was pasted", None)),
        }
    }
}

/// One read of Claude's composer immediately before the paste (zerus-7cq23):
/// an empty prompt, or why not with that frame's own text as the evidence.
pub(in crate::state) fn before_paste(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim) -> Before {
    let Some((screen, x, y)) = frame(terminal, claim) else {
        return Before::Moving;
    };
    let composer = || composer_text(claim.agent, &screen, y).filter(|text| !text.is_empty());
    if !composer_empty_for_model(claim.agent, &screen, x, y, None) {
        return Before::Refused("Claude's composer was not empty right before the paste; nothing was pasted", composer());
    }
    if cut(claim.agent, &screen, y) {
        return Before::Refused("Claude's pane became too short to show its whole composer; nothing was pasted", composer());
    }
    Before::Empty
}

/// Waits until a frame of the composer holds still, at most `BEFORE_PASTE`.
/// The caller holds no lock meanwhile and reads the composer again under it
/// (zerus-oz7cz).
///
/// Under the lock again the send checks that its typing marker and run are
/// still its own (`typing_ours`) and calls `terminating::check_mutation`.
/// No production writer sets that termination marker today, so the live
/// fences for a pause, kill or rename in this window are, as in the unlocked
/// settle after the paste (zerus-szz00): the marker and run check; this
/// composer read under the lock, whose input claim pins the pane's run marker
/// and session name and which a stopped or replaced agent does not pass as an
/// empty Claude prompt; the paste failing on a pane that is gone; and the
/// process and pane checks before the one Enter.
pub(in crate::state) fn hold_still(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim) {
    let deadline = Instant::now() + BEFORE_PASTE;
    while frame(terminal, claim).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One frame of the composer: its text when it shows a settled paste
/// (recognized, not empty, no `Pasting…`), and its text at all. None when
/// the cursor moved during the read. A composer taller than its view settles
/// only when its rows prove it holds exactly `pasted` (zerus-33caz); one with
/// rows out of view below the cursor never does.
fn settled(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim, pasted: &str, complete: bool) -> Option<(Option<String>, Option<String>)> {
    let (screen, x, y) = frame(terminal, claim)?;
    let text = if complete { composer_full(claim.agent, &screen, y) } else { composer_text(claim.agent, &screen, y) };
    let text = text.filter(|text| !text.is_empty());
    let rows: Vec<String> = screen.lines().map(plain).collect();
    let holds = match (claim.agent == "claude").then(|| claude_view(&rows, y)).flatten() {
        Some(View::Scrolled(view)) => view.holds(pasted, x, y),
        Some(View::Whole { below, .. }) => below == Some(0) && !composer_empty_for_model(claim.agent, &screen, x, y, None),
        None => !composer_empty_for_model(claim.agent, &screen, x, y, None),
    };
    let shown = text.clone().filter(|_| !pasting(&screen) && holds);
    Some((shown, text))
}

/// Whether the composer still shows exactly `text`, settled, for the paste of
/// `pasted`: the check made under the state lock immediately before the one
/// Enter (zerus-iitw8.5).
pub(in crate::state) fn still(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim, pasted: &str, text: &str) -> bool {
    settled(terminal, claim, pasted, false).and_then(|(shown, _)| shown).as_deref() == Some(text)
}

/// Pending-Send's Enter fence compares the complete captured composer;
/// the ordinary lane and bounded receipt previews retain their existing form.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn still_complete(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim, pasted: &str, text: &str) -> bool {
    settled(terminal, claim, pasted, true).and_then(|(shown, _)| shown).as_deref() == Some(text)
}

/// Waits until the composer shows the paste of `pasted` settled: recognized,
/// not empty, no `Pasting…`, and the same text on reads at least `STABLE`
/// apart. Ok holds that text; Err the composer's last text when `deadline`
/// passes first. Only the terminal is read, so the caller holds no lock
/// meanwhile.
pub(in crate::state) fn settle(
    terminal: &dyn backend::TerminalBackend,
    claim: &backend::InputClaim,
    pasted: &str,
    deadline: Instant,
) -> std::result::Result<String, Option<String>> {
    wait_settled(terminal, claim, pasted, deadline, false)
}

/// Same stable-read window, with no truncation in the pending-Send comparator.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::state) fn settle_complete(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim, pasted: &str, deadline: Instant) -> std::result::Result<String, Option<String>> {
    wait_settled(terminal, claim, pasted, deadline, true)
}

fn wait_settled(terminal: &dyn backend::TerminalBackend, claim: &backend::InputClaim, pasted: &str, deadline: Instant, complete: bool) -> std::result::Result<String, Option<String>> {
    let mut seen: Option<(String, Instant)> = None;
    let mut last = None;
    loop {
        if let Some((shown, text)) = settled(terminal, claim, pasted, complete) {
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
            return Err(last.map(|text: String| text.chars().take(RESIDUE_CHARS).collect()));
        }
        std::thread::sleep(POLL);
    }
}

/// Claude's transcript from its length before the paste: where the proof of
/// this submission must appear. Each read continues after the last complete
/// line (zerus-iitw8.6), at most `TRANSCRIPT_BYTES` after the start in all.
pub(in crate::state) struct Transcript {
    path: PathBuf,
    // Hold the selected object against inode reuse. Appends change timestamps.
    source: Option<(fs::File, (u64, u64))>,
    length: u64,
    unavailable: bool,
    /// Where the next read starts: the start plus every complete line read.
    next: u64,
    /// How many bytes after the start have been read.
    read: u64,
    /// The last line read without its newline yet.
    partial: Vec<u8>,
}

fn transcript_source(
    path: &std::path::Path,
) -> std::io::Result<(fs::File, (u64, u64), fs::Metadata)> {
    let file = crate::platform::no_follow(fs::OpenOptions::new().read(true)).open(path)?;
    let (identity, metadata) = crate::platform::FileIdentity::with_metadata(&file)?;
    if !metadata.is_file() || identity.inode == 0 {
        return Err(std::io::Error::other(
            "Claude transcript is not a regular identifiable file",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err(std::io::Error::other(
                "Claude transcript is a reparse point",
            ));
        }
    }
    Ok((file, (identity.device, identity.inode), metadata))
}

impl Transcript {
    pub(in crate::state) fn at(path: PathBuf, start: u64) -> Self {
        let opened = transcript_source(&path);
        let unavailable = opened
            .as_ref()
            .err()
            .is_some_and(|e| e.kind() != std::io::ErrorKind::NotFound);
        let (source, length) = opened
            .map(|(file, id, metadata)| (Some((file, id)), metadata.len()))
            .unwrap_or_default();
        Transcript {
            path,
            source,
            length,
            unavailable,
            next: start,
            read: 0,
            partial: Vec::new(),
        }
    }

    /// The transcript the record names, from its length now. None when the
    /// record names none yet.
    pub(in crate::state) fn of(record: &Value) -> Option<Self> {
        let path = PathBuf::from(string(record, "transcript"));
        if !path.is_absolute() {
            return None;
        }
        let mut transcript = Transcript::at(path, 0);
        if !super::acceptance::allows_pending(record) {
            transcript.unavailable = true;
        }
        transcript.next = transcript.length;
        // A named but unreadable source stays present: never fall back to a
        // composer's empty state as proof of this send.
        Some(transcript)
    }

    /// Where the next read starts (WhiteWh fork: the receipt keeps it for a
    /// later proof).
    pub(super) fn position(&self) -> u64 {
        self.next
    }

    /// Complete appended rows, or unknown physical evidence. A failed read
    /// never consumes a row or becomes an empty-transcript success.
    fn rows(&mut self) -> Option<Vec<Value>> {
        if self.unavailable {
            return None;
        }
        let rows = self.read_rows();
        if rows.is_none() && self.source.is_some() {
            // Once selected, a changed/unreadable source cannot be adopted by
            // a later poll of this send. Genuinely absent new files may appear.
            self.unavailable = true;
        }
        rows
    }

    fn read_rows(&mut self) -> Option<Vec<Value>> {
        let budget = TRANSCRIPT_BYTES.saturating_sub(self.read);
        if budget == 0 {
            return None;
        }
        let (mut file, source, metadata) = transcript_source(&self.path).ok()?;
        if self
            .source
            .as_ref()
            .is_some_and(|(_, selected)| *selected != source)
            || metadata.len() < self.length
        {
            return None;
        }
        if self.source.is_none() {
            self.source = Some((file.try_clone().ok()?, source));
        }
        let mut bytes = Vec::new();
        let at = self.next.checked_add(self.partial.len() as u64)?;
        if metadata.len() < at {
            return None;
        }
        file.seek(SeekFrom::Start(at)).ok()?;
        (&mut file).take(budget).read_to_end(&mut bytes).ok()?;
        #[cfg(test)]
        AFTER_TRANSCRIPT_READ.with(|slot| {
            if let Some(hook) = slot.take() {
                hook(&self.path);
            }
        });
        let (_, named, _) = transcript_source(&self.path).ok()?;
        let length = file.metadata().ok()?.len();
        if named != source || length < at.checked_add(bytes.len() as u64)? {
            return None;
        }
        self.length = length;
        self.read += bytes.len() as u64;
        self.partial.extend_from_slice(&bytes);
        let Some(end) = self.partial.iter().rposition(|&b| b == b'\n') else {
            return Some(Vec::new());
        };
        let complete: Vec<u8> = self.partial.drain(..=end).collect();
        self.next += complete.len() as u64;
        Some(
            complete
                .split(|&b| b == b'\n')
                .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
                .collect(),
        )
    }

    /// The first new row that records `message` as submitted or queued.
    fn proof(&mut self, message: &str, pasted_at: f64) -> Option<Proof> {
        self.rows()?
            .iter()
            .find_map(|row| row_proof(row, message, pasted_at))
    }
}

/// What proved the submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::state) enum Proof {
    /// A new typed user row of the transcript is the message.
    Transcript,
    /// Claude queued it while busy: a new transcript `enqueue` is the message.
    Queued,
    /// No transcript was known yet (a new conversation's first message): the
    /// settled composer emptied after the Enter.
    Composer,
}

impl Proof {
    pub(in crate::state) fn name(self) -> &'static str {
        match self {
            Proof::Transcript => "transcript",
            Proof::Queued => "queue",
            Proof::Composer => "composer",
        }
    }

    /// Whether Claude itself recorded the message, in its transcript or its
    /// queue. An emptied composer alone does not show that it accepted it.
    pub(in crate::state) fn recorded(self) -> bool {
        matches!(self, Proof::Transcript | Proof::Queued)
    }
}

/// Waits for Claude's own record of the submission until `deadline`. Err
/// holds the composer's text then, as evidence. `pasted_at` is the wall clock
/// just before the paste: no proving row may be older.
pub(in crate::state) fn confirm(
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

/// How much earlier than the receipt's start a transcript row may be stamped
/// and still count: the receipt is written before the paste on this computer,
/// so this only absorbs the stamps' rounding.
const LATE_SLACK: f64 = 2.0;

/// WhiteWh fork (owner, 2026-11-13): the window's proof read after the window,
/// for a delivery left uncertain. The first row from `offset` on, stamped at
/// `since` or later, that records `message` as typed or queued (`row_proof`).
/// The transcript is read line by line without holding it, and only lines
/// holding the start of the message's first line are parsed.
pub(super) fn late_proof(path: &Path, offset: u64, message: &str, since: f64) -> Option<Proof> {
    use std::io::BufRead;
    let mut file = fs::File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(if offset <= length { offset } else { 0 })).ok()?;
    let needle = late_needle(message);
    for line in std::io::BufReader::new(file).split(b'\n') {
        let line = line.ok()?;
        if !needle.is_empty() && !line.windows(needle.len()).any(|window| window == needle.as_bytes()) {
            continue;
        }
        let Ok(row) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if let Some(proof) = row_proof(&row, message, since - LATE_SLACK) {
            return Some(proof);
        }
    }
    None
}

/// The start of the message's first significant line as Claude's transcript
/// writes it unescaped: up to 24 characters before a quote, backslash or
/// control character. Empty when that leaves too little to filter by.
fn late_needle(message: &str) -> String {
    let first = significant(message).into_iter().next().unwrap_or_default();
    let needle: String = first
        .chars()
        .take_while(|ch| !matches!(ch, '"' | '\\') && !ch.is_control())
        .take(24)
        .collect();
    if needle.chars().count() < 4 {
        String::new()
    } else {
        needle
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
pub(in crate::state) fn significant(text: &str) -> Vec<String> {
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
pub(in crate::state) fn same(message: &str, recorded: &str) -> bool {
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_changed_composer_tail_cannot_hide_behind_the_evidence_bound() {
        let prefix = "x".repeat(RESIDUE_CHARS);
        let before = format!("{prefix} original");
        let after = format!("{prefix} changed");
        let terminal = Frames::new(vec![(screen(&after, ""), 2, 2)]);
        assert!(!still_complete(&terminal, &claim(), &before, &before));
        // The displayed composer, not its evidence preview, is the fence.
        let terminal = Frames::new(vec![(screen(&before, ""), 2, 2)]);
        assert!(still_complete(&terminal, &claim(), &before, &before));
        assert_eq!(composer_text("claude", &screen(&before, ""), 2).unwrap().chars().count(), RESIDUE_CHARS);
    }


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

    /// A pane whose cursor reads from its fields while `input_state` (the
    /// console's full evidence) refuses, as a supervisor under load does.
    struct Fields(Frames);
    impl backend::TerminalBackend for Fields {
        fn pane_info(&self, _target: backend::Target, _fields: &[backend::PaneField]) -> Result<backend::PaneInfo> {
            let (_, x, y) = self.0.current();
            Ok(backend::PaneInfo::new(format!("{x}\t{y}\n")))
        }
        fn input_state(&self, _claim: &backend::InputClaim) -> Result<backend::InputState> {
            Err("console evidence timed out".into())
        }
        fn capture(&self, pane: &str, mode: backend::Capture) -> Result<Vec<u8>> {
            self.0.capture(pane, mode)
        }
    }

    #[test]
    fn the_settle_reads_the_panes_cursor_without_the_consoles_evidence() {
        // WhiteWh fork (owner, 2026-11-13): under a build's load the evidence
        // read timed out and a delivered paste stayed unsent.
        let settled = (screen("Reply with just OK.", "⏵⏵ bypass permissions on"), 21, 2);
        let terminal = Fields(Frames::new(vec![settled]));
        assert!(settle(&terminal, &claim(), "Reply with just OK.", soon(2_000)).is_ok());
        assert_eq!(residue(&terminal, &claim()).as_deref(), Some("Reply with just OK."));
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
        // A layout without rules ends at its first row that is blank or not
        // indented as Claude's continuation rows are (zerus-szz00).
        assert_eq!(composer_text("claude", "❯ echoed\n  second\n\nolder output\n", 1).as_deref(), Some("echoed\nsecond"));
        assert_eq!(composer_text("claude", "❯ echoed\nsecond\n\nolder output\n", 1).as_deref(), Some("echoed"));
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
        assert_eq!(settle(&frames, &claim(), "message", soon(3_000)), Ok(SETTLED.into()));
        assert!(start.elapsed() >= STABLE, "the settled text must hold for {STABLE:?}");
        // Even a composer settled from the first read holds for the whole window.
        let frames = Frames::new(vec![(screen(SETTLED, "paste again to expand"), 50, 2)]);
        let start = Instant::now();
        assert_eq!(settle(&frames, &claim(), "message", soon(3_000)), Ok(SETTLED.into()));
        assert!(start.elapsed() >= STABLE, "entered after {:?}", start.elapsed());
    }

    // zerus-33caz: Claude Code 2.1.326 screens captured from the private real
    // TUI at 80x24 (tmux's size for a session no client has attached to) with
    // this message pasted as hgs pastes it (`paste-buffer -p -r`). The message
    // is taller than the composer: its last rows show under `─── ↑ 3 more ───`.
    const TALL: &str = include_str!("../../../tests/fixtures/claude-composer-scrolled-message.txt");
    /// The owner's shape: the earlier prompt rows are out of view.
    const SCROLLED: &str = include_str!("../../../tests/fixtures/claude-composer-scrolled-2.1.326.txt");
    /// A past prompt `❯ Hello fixture.` is still in view above the bar.
    const SCROLLED_HISTORY: &str = include_str!("../../../tests/fixtures/claude-composer-scrolled-history-2.1.326.txt");
    /// Pasted while Claude works, its past prompt in view, and after its Stop.
    const SCROLLED_BUSY: &str = include_str!("../../../tests/fixtures/claude-composer-scrolled-busy-2.1.326.txt");
    const SCROLLED_STOP: &str = include_str!("../../../tests/fixtures/claude-composer-scrolled-stop-2.1.326.txt");
    /// The cursor moved up one row, three rows up (`↑ 2 more` and `↓ 1 more`),
    /// and to the prompt row with the last rows hidden below (`↓ 3 more`).
    const SCROLLED_MOVED: &str = include_str!("../../../tests/fixtures/claude-composer-scrolled-moved-2.1.326.txt");
    const SCROLLED_BOTH: &str = include_str!("../../../tests/fixtures/claude-composer-scrolled-both-2.1.326.txt");
    const HIDDEN_BELOW: &str = include_str!("../../../tests/fixtures/claude-composer-hidden-below-2.1.326.txt");

    fn settle_tall(frames: &Frames, text: &str) -> std::result::Result<String, Option<String>> {
        settle(frames, &claim(), text, soon(1_000))
    }
    fn still_tall(frames: &Frames, text: &str, shown: &str) -> bool {
        still(frames, &claim(), text, shown)
    }

    #[test]
    fn a_message_taller_than_the_composer_settles_on_this_paste() {
        for (name, screen) in [("scrolled", SCROLLED), ("history", SCROLLED_HISTORY), ("busy", SCROLLED_BUSY), ("stop", SCROLLED_STOP)] {
            let frames = Frames::new(vec![(screen.to_owned(), 18, 21)]);
            let shown = settle_tall(&frames, TALL).unwrap_or_else(|composer| panic!("{name}: never settled: {composer:?}"));
            // Never a past prompt in the conversation above the composer.
            assert!(shown.ends_with("</zerus-message>"), "{name}: {shown:?}");
            assert!(still_tall(&frames, TALL, &shown), "{name}");
        }
    }

    #[test]
    fn a_scrolled_composer_is_evidence_of_its_shown_rows() {
        for screen in [SCROLLED, SCROLLED_HISTORY] {
            let text = composer_text("claude", screen, 21).expect("the shown composer rows");
            assert!(text.starts_with("↑ 3 more\n11111111-2222-4333-8444-555555555555&amp;run="), "{text:?}");
            assert!(text.ends_with("Вопрос закрыт.\n</zerus-message>"), "{text:?}");
        }
    }

    #[test]
    fn a_scrolled_composer_settles_only_on_exactly_this_paste() {
        let frames = |screen: &str, x: usize, y: usize| Frames::new(vec![(screen.to_owned(), x, y)]);
        // Not where the paste leaves it: the cursor one row up, rows out of
        // view below the cursor, or the prompt row shown over rows out of view.
        for (name, screen, x, y) in [("moved", SCROLLED_MOVED, 16, 20), ("both", SCROLLED_BOTH, 16, 18), ("below", HIDDEN_BELOW, 9, 15)] {
            assert!(settle_tall(&frames(screen, x, y), TALL).is_err(), "{name}");
        }
        // The owner's screen with the cursor reported a cell left of the end
        // or a row above it: not after this paste's last character.
        for (x, y) in [(17, 21), (18, 20)] {
            assert!(settle_tall(&frames(SCROLLED, x, y), TALL).is_err(), "{x},{y}");
        }
        // Rows of another text: one more or fewer row out of view, rows that
        // go on after the shown ones, text typed after the paste at the
        // cursor, another shown row, an empty last row.
        let shown = composer_text("claude", SCROLLED, 21).unwrap();
        for text in [
            format!("Hello.\n{TALL}"),
            format!("{TALL}\n</zerus-message>"),
            TALL.split_once('\n').unwrap().1.to_owned(),
            format!("{TALL} and more"),
            TALL.replace("Вопрос закрыт.", "Вопрос открыт."),
            format!("{TALL}\n"),
            String::new(),
        ] {
            assert!(settle_tall(&frames(SCROLLED, 18, 21), &text).is_err(), "{text:?}");
            assert!(!still_tall(&frames(SCROLLED, 18, 21), &text, &shown), "{text:?}");
        }
        // The bar and Claude's closing rule are the pane's width: another
        // width is another layout.
        let rows: Vec<&str> = SCROLLED.lines().collect();
        let closing = rows.iter().rposition(|row| separator(&plain(row))).unwrap();
        let mut narrowed = rows.clone();
        let shorter = rows[closing].replacen('─', "", 1);
        narrowed[closing] = &shorter;
        assert!(settle_tall(&frames(&narrowed.join("\n"), 18, 21), TALL).is_err());
        // A paste still being processed is not settled.
        let processing = SCROLLED.replace("⏵⏵ bypass permissions on", "Pasting…");
        assert!(pasting(&processing));
        assert!(settle_tall(&frames(&processing, 18, 21), TALL).is_err());
    }

    // zerus-7cq23: Claude Code 2.1.327 screens from the private real TUI at
    // 80x24, pasted as hgs pastes. This message's middle line is empty, which
    // Claude shows as a blank row inside its composer. With the cursor moved
    // up until the prompt row is in view, the rows after it are out of view
    // under `─── ↓ 1 more ───`; the cursor is above or below the blank row.
    const BLANK: &str = include_str!("../../../tests/fixtures/claude-composer-blank-message.txt");
    const BLANK_HIDDEN_BELOW: &str = include_str!("../../../tests/fixtures/claude-composer-blank-hidden-below-2.1.327.txt");
    const BLANK_HIDDEN_BELOW_UNDER: &str = include_str!("../../../tests/fixtures/claude-composer-blank-hidden-below-under-2.1.327.txt");
    /// The first of those after the pane shrank to 11 rows: the composer's
    /// later rows, its rule and the footer are below the screen's last row.
    const BLANK_CUT: &str = include_str!("../../../tests/fixtures/claude-composer-blank-cut-2.1.327.txt");
    /// A short message with an empty middle line, whole in its composer.
    const BLANK_SHORT: &str = include_str!("../../../tests/fixtures/claude-composer-blank-short-2.1.327.txt");
    /// This message pasted in front of a person's draft row (`\nEXTRA`, the
    /// cursor at its start), then the cursor walked up three rows:
    /// `─── ↑ 3 more ───`, this message's last seven rows, and the draft row
    /// out of view under `─── ↓ 1 more ───`.
    const SCROLLED_BOTH_DRAFT: &str = include_str!("../../../tests/fixtures/claude-composer-scrolled-both-2.1.327.txt");

    #[test]
    fn a_composer_in_full_view_settles_with_every_row_up_to_its_rule() {
        let frames = Frames::new(vec![(BLANK_SHORT.to_owned(), 34, 21)]);
        let message = "Short first line of the fixture.\n\nShort third line of the fixture.";
        assert_eq!(settle_tall(&frames, message).as_deref(), Ok(message));
        assert!(still_tall(&frames, message, message));
        // The row after the blank one is part of the Enter fence.
        let changed = BLANK_SHORT.replace("Short third line", "Short other line");
        assert!(!still_tall(&Frames::new(vec![(changed, 34, 21)]), message, message));
    }

    #[test]
    fn a_composer_in_full_view_with_rows_out_of_view_never_settles() {
        let settled: Vec<&str> = [
            ("cursor above the blank row", BLANK_HIDDEN_BELOW, 25, 16),
            ("cursor below the blank row", BLANK_HIDDEN_BELOW_UNDER, 71, 18),
            ("cut off by the pane", BLANK_CUT, 25, 9),
        ]
        .into_iter()
        .filter(|(_, screen, x, y)| settle_tall(&Frames::new(vec![(screen.to_string(), *x, *y)]), BLANK).is_ok())
        .map(|(name, ..)| name)
        .collect();
        assert!(settled.is_empty(), "settled with rows out of view: {settled:?}");
        // Hand-edited: an `↑` bar where Claude draws its `↓` one is no layout
        // of its composer, and never settles either.
        let upward = BLANK_HIDDEN_BELOW.replacen("↓ 1 more", "↑ 1 more", 1);
        assert_ne!(upward, BLANK_HIDDEN_BELOW);
        assert!(settle_tall(&Frames::new(vec![(upward, 25, 16)]), BLANK).is_err());
        // The evidence reads past the blank row and counts the hidden rows.
        let text = composer_text("claude", BLANK_HIDDEN_BELOW, 16).unwrap();
        assert!(text.starts_with("Paragraph one of the fixture draft"), "{text:?}");
        assert!(text.contains("here.\n\nParagraph three"), "{text:?}");
        assert!(text.ends_with("so that the wrapped\n↓ 1 more"), "{text:?}");
    }

    #[test]
    fn a_scrolled_composer_with_rows_out_of_view_below_never_settles() {
        let frames = |x: usize, y: usize| Frames::new(vec![(SCROLLED_BOTH_DRAFT.to_owned(), x, y)]);
        // Where Claude left its cursor: the middle row of the view.
        assert!(settle_tall(&frames(18, 18), TALL).is_err());
        // Hand-set cursor (Claude 2.1.327 keeps it on the middle row of a
        // view with rows below, so it never drew this): right after this
        // paste's last character on the last shown row. Everything else then
        // matches this paste: the bar's three rows and the seven shown are its
        // ten, and the shown ones are its last seven. Only the draft row out
        // of view below refuses it.
        assert!(settle_tall(&frames(18, 21), TALL).is_err());
        let rows: Vec<&str> = SCROLLED_BOTH_DRAFT.lines().collect();
        let bar = rows.iter().position(|row| more_bar(&plain(row)) == Some((false, 1))).unwrap();
        let rule = "─".repeat(80);
        let mut closed = rows.clone();
        closed[bar] = &rule;
        let closed = closed.join("\n");
        assert_eq!(settle_tall(&Frames::new(vec![(closed, 18, 21)]), TALL).map(|_| ()), Ok(()), "control: the same rows over Claude's rule");
        assert!(composer_text("claude", SCROLLED_BOTH_DRAFT, 21).unwrap().ends_with("</zerus-message>\n↓ 1 more"));
    }

    /// zerus-7cq23: Claude Code 2.1.327 idle at 80x24, its prompt empty and
    /// with `x` typed into it.
    const EMPTY: &str = include_str!("../../../tests/fixtures/claude-composer-empty-2.1.327.txt");
    const DRAFT: &str = include_str!("../../../tests/fixtures/claude-composer-draft-2.1.327.txt");

    /// A terminal whose cursor moves on every read.
    struct Moving(std::sync::atomic::AtomicUsize);
    impl backend::TerminalBackend for Moving {
        fn input_state(&self, _claim: &backend::InputClaim) -> Result<backend::InputState> {
            let x = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(backend::InputState { cursor_x: x.to_string(), cursor_y: "21".into() })
        }
        fn capture(&self, _pane: &str, _mode: backend::Capture) -> Result<Vec<u8>> {
            Ok(EMPTY.as_bytes().to_vec())
        }
    }

    /// The refusal and evidence of one read right before the paste.
    fn before(terminal: &dyn backend::TerminalBackend) -> Option<(&'static str, Option<String>)> {
        before_paste(terminal, &claim()).refused()
    }

    #[test]
    fn the_composer_is_read_empty_right_before_the_paste_only_on_an_empty_prompt() {
        let one = |screen: &str, x: usize, y: usize| Frames::new(vec![(screen.to_owned(), x, y)]);
        assert_eq!(before(&one(EMPTY, 2, 21)), None);
        // A person's draft, and a composer holding a message out of view.
        let (reason, composer) = before(&one(DRAFT, 3, 21)).unwrap();
        assert!(reason.contains("was not empty right before the paste; nothing was pasted"), "{reason}");
        assert_eq!(composer.as_deref(), Some("x"));
        let (_, composer) = before(&one(SCROLLED, 18, 21)).unwrap();
        assert!(composer.unwrap().starts_with("↑ 3 more\n"));
        // A frame whose cursor moved while it was read decides nothing: the
        // send waits for one that holds still, then reads the composer again.
        let frames = |first: (&str, usize), then: (&str, usize)| {
            Frames::new(vec![(first.0.to_owned(), first.1, 21), (then.0.to_owned(), then.1, 21)])
        };
        for (first, then, refused) in [((DRAFT, 3), (EMPTY, 2), false), ((EMPTY, 2), (DRAFT, 3), true)] {
            let terminal = frames(first, then);
            assert!(matches!(before_paste(&terminal, &claim()), Before::Moving));
            hold_still(&terminal, &claim());
            assert_eq!(before(&terminal).is_some(), refused, "{first:?} then {then:?}");
        }
        // One that never holds still is refused, with no frame as evidence,
        // and the unlocked wait for it is bounded.
        let terminal = Moving(0.into());
        let (reason, composer) = before(&terminal).unwrap();
        assert!(reason.contains("did not hold still right before the paste; nothing was pasted"), "{reason}");
        assert_eq!(composer, None);
        let start = Instant::now();
        hold_still(&terminal, &claim());
        assert!(start.elapsed() >= BEFORE_PASTE, "{:?}", start.elapsed());
        assert!(start.elapsed() < BEFORE_PASTE * 4, "{:?}", start.elapsed());
        // A frame that holds still ends the wait at once.
        let start = Instant::now();
        hold_still(&one(EMPTY, 2, 21), &claim());
        assert!(start.elapsed() < BEFORE_PASTE, "{:?}", start.elapsed());
    }

    /// The real draft frame, read once with the cursor still; every later
    /// read sees the cursor move, as when the person keeps typing.
    struct TypingOn(std::sync::atomic::AtomicUsize);
    impl backend::TerminalBackend for TypingOn {
        fn input_state(&self, _claim: &backend::InputClaim) -> Result<backend::InputState> {
            let read = self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(backend::InputState { cursor_x: if read < 2 { 3 } else { read }.to_string(), cursor_y: "21".into() })
        }
        fn capture(&self, _pane: &str, _mode: backend::Capture) -> Result<Vec<u8>> {
            Ok(DRAFT.as_bytes().to_vec())
        }
    }

    #[test]
    fn a_refusal_right_before_the_paste_keeps_the_text_of_the_frame_that_refused() {
        // zerus-oz7cz: the person keeps typing after the read that refused.
        // Another read now has no frame, so the refusal's own is the evidence.
        let terminal = TypingOn(0.into());
        assert_eq!(before(&terminal).unwrap().1.as_deref(), Some("x"));
        assert_eq!(residue(&terminal, &claim()), None);
    }

    #[test]
    fn a_composer_without_a_closing_rule_keeps_the_conversation_out_of_its_evidence() {
        // zerus-szz00, hand-made: a draft on the prompt row and the
        // conversation right under it, no rule. Only the composer's rows are
        // evidence, in the receipt and in the refusal before the paste.
        let screen = "❯ x\n● Fixture complete.\n  continued reply\n";
        assert_eq!(composer_text("claude", screen, 0).as_deref(), Some("x"));
        assert_eq!(before(&Frames::new(vec![(screen.to_owned(), 3, 0)])).unwrap().1.as_deref(), Some("x"));
        // Claude's own continuation rows, indented, are still the composer's.
        assert_eq!(composer_text("claude", "❯ first\n  second\n● Done.\n", 0).as_deref(), Some("first\nsecond"));
    }

    // zerus-oz7cz: Claude Code 2.1.327 from the private real TUI after its first
    // turn, its pane shrunk from 80x24 to 8 and to 7 rows. At 8 its closing
    // rule is the last row; at 7 the prompt row is, and the rule is cut off.
    const EMPTY_8: &str = include_str!("../../../tests/fixtures/claude-composer-empty-8-rows-2.1.327.txt");
    const EMPTY_7: &str = include_str!("../../../tests/fixtures/claude-composer-empty-7-rows-2.1.327.txt");
    /// `Short single line of the fixture.` pasted as hgs pastes, and a
    /// two-line message Claude shows as a placeholder in a pane this short.
    const PASTED_8: &str = include_str!("../../../tests/fixtures/claude-composer-pasted-8-rows-2.1.327.txt");
    const PLACEHOLDER_8: &str = include_str!("../../../tests/fixtures/claude-composer-placeholder-8-rows-2.1.327.txt");
    const PASTED_7: &str = include_str!("../../../tests/fixtures/claude-composer-pasted-7-rows-2.1.327.txt");

    #[test]
    fn a_pane_too_short_for_the_composers_rule_is_refused_before_anything_is_sent() {
        let one = |screen: &str, x: usize, y: usize| Frames::new(vec![(screen.to_owned(), x, y)]);
        let record = json!({"agent": "claude", "name": "claude/p/s", "run_id": "r", "pane": "%1", "activity": "idle", "phase": "idle"});
        // 7 rows: the send gate refuses before any receipt, with its sentence.
        assert!(cut("claude", EMPTY_7, 6));
        match terminal_gate_on(&record, true, true, &one(EMPTY_7, 2, 6)) {
            Err(Gate::Other(reason)) => assert_eq!(reason, SHORT),
            other => panic!("{other:?}"),
        }
        // A pane that shrank after the gate: refused right before the paste.
        let (reason, composer) = before(&one(EMPTY_7, 2, 6)).unwrap();
        assert!(reason.contains("too short to show its whole composer; nothing was pasted"), "{reason}");
        assert_eq!(composer, None);
        // A paste there never settles: a shrink during the settle leaves it.
        let single = "Short single line of the fixture.";
        assert!(settle_tall(&one(PASTED_7, 35, 6), single).is_err());
        // 8 rows still sends: gate, read before the paste, settle and fence.
        assert!(!cut("claude", EMPTY_8, 6));
        assert!(terminal_gate_on(&record, true, true, &one(EMPTY_8, 2, 6)).is_ok());
        assert_eq!(before(&one(EMPTY_8, 2, 6)), None);
        assert_eq!(settle_tall(&one(PASTED_8, 35, 6), single).as_deref(), Ok(single));
        assert!(still_tall(&one(PASTED_8, 35, 6), single, single));
        let two = "Short first line of the fixture.\nShort third line of the fixture.";
        assert_eq!(settle_tall(&one(PLACEHOLDER_8, 27, 6), two).as_deref(), Ok("[Pasted text #1 +1 lines]"));
        // So does a full pane, and the check is Claude's alone.
        assert!(!cut("claude", EMPTY, 21));
        assert!(!cut("codex", EMPTY_7, 6));
        // Hand-made, as the fake agent of tests/test_input.py draws Claude: a
        // prompt with blank rows under it and no rule is no pane's edge. It is
        // not refused as too short; a paste there still never settles.
        let bare = format!("❯ {}", "\n".repeat(23));
        assert!(!cut("claude", &bare, 0));
        assert!(terminal_gate_on(&record, true, true, &one(&bare, 2, 0)).is_ok());
        assert_eq!(before(&one(&bare, 2, 0)), None);
        // The same prompt on the screen's last row is the pane's edge.
        let edge = format!("{}❯ ", "\n".repeat(23));
        assert!(cut("claude", &edge, 23));
    }

    #[test]
    fn claudes_composer_rows_are_the_rows_its_screens_show() {
        // Rows and cursors Claude Code 2.1.326 itself showed for these texts.
        let corpus: Value = serde_json::from_str(include_str!("../../../tests/fixtures/claude-composer-layout-2.1.326.json")).unwrap();
        let cases = corpus["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 64);
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let columns = case["columns"].as_u64().unwrap() as usize;
            let rows = claude_rows(case["text"].as_str().unwrap(), columns - GUTTER * 2).unwrap_or_else(|| panic!("{name}"));
            let shown: Vec<&str> = rows.iter().map(|row| row.text.trim_end()).collect();
            let captured: Vec<&str> = case["rows"].as_array().unwrap().iter().map(|row| row.as_str().unwrap()).collect();
            assert_eq!(shown, captured, "{name} at {columns}");
            assert_eq!(case["cursor"], json!([GUTTER + rows.last().unwrap().cells, rows.len() - 1]), "{name} at {columns}");
        }
        // Text this does not measure is never proved.
        for text in ["tab\there", "carriage\rreturn", "e\u{301}", "zero\u{200b}width", "\u{1b}[1m"] {
            assert!(claude_rows(text, 76).is_none(), "{text:?}");
        }
        assert!(claude_rows("x", 0).is_none());
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
        assert_eq!(settle(&frames, &claim(), "message", soon(300)), Err(None));
        // An empty composer is not a settled paste either.
        let frames = Frames::new(vec![(screen("", "⏵⏵ bypass permissions on"), 2, 2)]);
        assert_eq!(settle(&frames, &claim(), "message", soon(300)), Err(None));
        // Text that keeps changing never settles; the last text is the evidence.
        let changing: Vec<_> = (0..40).map(|n| (screen(&format!("draft {n}"), "x"), 10, 2)).collect();
        assert!(matches!(settle(&Frames::new(changing), &claim(), "message", soon(300)), Err(Some(text)) if text.starts_with("draft")));
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
    fn unreadable_named_transcripts_never_fall_back_to_an_empty_composer() {
        let dir = tempfile::tempdir().unwrap();
        for path in [dir.path().to_owned(), dir.path().join("missing.jsonl")] {
            let mut transcript = Transcript::of(&json!({"transcript":path}));
            let frames = Frames::new(vec![(screen("", "ready"), 2, 2)]);
            assert!(confirm(
                &frames,
                &claim(),
                transcript.as_mut(),
                "message",
                0.0,
                soon(120)
            )
            .is_err());
        }
    }

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
    fn a_late_proof_counts_rows_stamped_after_the_receipt() {
        // WhiteWh fork (owner, 2026-11-13): an uncertain delivery that Claude
        // recorded after the window closed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let row = |at: &str, kind: &str| match kind {
            "user" => json!({"type":"user","timestamp":at,"message":{"content":MESSAGE}}),
            "queued" => json!({"type":"queue-operation","operation":"enqueue","timestamp":at,"content":MESSAGE}),
            _ => json!({"type":"user","message":{"content":MESSAGE}}),
        };
        // 2026-11-12T20:35:58Z
        let since = 1_794_515_758.0;
        let earlier = row("2026-11-12T20:30:00.000Z", "user");
        let unstamped = row("", "none");
        let other = json!({"type":"user","timestamp":"2026-11-12T20:36:00.000Z","message":{"content":"another message"}});
        fs::write(&path, format!("{earlier}\n{unstamped}\n{other}\n")).unwrap();
        assert_eq!(late_proof(&path, 0, MESSAGE, since), None, "an earlier, an unstamped and another row");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", row("2026-11-12T20:35:59.120Z", "queued")).unwrap();
        assert_eq!(late_proof(&path, 0, MESSAGE, since), Some(Proof::Queued));
        let offset = fs::metadata(&path).unwrap().len();
        assert_eq!(late_proof(&path, offset, MESSAGE, since), None, "rows before the offset");
        writeln!(file, "{}", row("2026-11-12T20:40:00.000Z", "user")).unwrap();
        assert_eq!(late_proof(&path, offset, MESSAGE, since), Some(Proof::Transcript));
        // An offset past a rewritten transcript reads it from the start.
        assert_eq!(late_proof(&path, offset * 10, MESSAGE, since), Some(Proof::Queued));
        assert_eq!(late_proof(&dir.path().join("missing.jsonl"), 0, MESSAGE, since), None);
    }

    #[test]
    fn the_late_needle_is_the_unescaped_start_of_the_first_line() {
        assert_eq!(late_needle(MESSAGE), "<zerus-message from=");
        assert_eq!(late_needle("так, а сам svn готов получается?"), "так, а сам svn готов пол");
        assert_eq!(late_needle("[Image #1] ok"), "", "too short to filter by");
        assert_eq!(late_needle("\"quoted\""), "");
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
