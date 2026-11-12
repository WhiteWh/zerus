//! Kimi 2.x native question-dialog bridge. This is deliberately a terminal
//! adapter, not a claim that a TUI exposes KAP's in-process question service.
//! Each small action checks the live question and screen; only a matching
//! interaction.resolved event acknowledges delivery. Never automatically replay
//! input; explicit draft recovery verifies the full review before submission.
use super::*;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const REVIEW: &str = "Review your answer before submit";
const READY: &str = "Ready to submit your answers?";
const EDITING: &str = "Type your answer, then press Enter to save.";

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The tallest pane `fit_unviewed` grows: tmux's default 24 rows doubled three times.
pub(super) const MAX_UNVIEWED_ROWS: u16 = 192;
const REDRAW: Duration = Duration::from_secs(1);

/// Blank rows below the last drawn row of a pane `height` rows tall. Escape
/// sequences alone draw nothing.
fn blank_below(screen: &str, height: u16) -> usize {
    static ESCAPE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let escape = ESCAPE.get_or_init(|| {
        regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))").unwrap()
    });
    let rows: Vec<_> = screen.lines().collect();
    let drawn = rows
        .iter()
        .rposition(|row| !escape.replace_all(row, "").trim().is_empty())
        .map_or(0, |index| index + 1);
    usize::from(height).saturating_sub(drawn)
}

/// Agents keep no scrollback for a native panel taller than their pane, so in
/// a pane no client views, such as one still at tmux's default 80x24, the top
/// of the panel shows nowhere. While `clipped(screen)`, let that pane grow
/// until the agent redraws the panel and `complete(screen)`; the caller's exact
/// match still decides. Call it only for a request the agent records.
///
/// This holds the run's answer lock (`questions::answer_lock`) and gives up when
/// an answer holds it: it never resizes a pane an answer drives, and an answer
/// that starts meanwhile sees the redrawn panel.
pub(super) fn fit_unviewed(
    record: &Value,
    screen: String,
    capture: fn(&Value) -> Result<String>,
    clipped: fn(&str) -> bool,
    complete: fn(&str) -> bool,
) -> String {
    if !clipped(&screen) {
        return screen;
    }
    let Ok(path) = questions::answer_lock(string(record, "run_id")) else {
        return screen;
    };
    let Ok(Some(_answer)) = lock_until(&path, Instant::now()) else {
        return screen;
    };
    // An answer may have finished just before this lock was free.
    match capture(record) {
        Ok(screen) => grow_until(record, screen, capture, clipped, complete),
        Err(_) => screen,
    }
}

/// Double the height of an unviewed pane, width unchanged and at most
/// MAX_UNVIEWED_ROWS, waiting up to a second each time for the agent to
/// redraw, while its panel stays clipped. A viewed pane keeps its client's size.
pub(super) fn grow_until(
    record: &Value,
    mut screen: String,
    capture: fn(&Value) -> Result<String>,
    clipped: fn(&str) -> bool,
    complete: fn(&str) -> bool,
) -> String {
    let terminal = backend::current();
    let pane = string(record, "pane");
    let Some(mut height) = terminal
        .pane_info(backend::Target::Pane(pane), &[backend::PaneField::Height])
        .ok()
        .and_then(|info| info.value().parse::<u16>().ok())
    else {
        return screen;
    };
    while clipped(&screen) && height < MAX_UNVIEWED_ROWS {
        let gap = blank_below(&screen, height);
        let rows = height.saturating_mul(2).min(MAX_UNVIEWED_ROWS);
        if !terminal.grow_unviewed(pane, rows).unwrap_or(false) {
            break;
        }
        height = rows;
        // tmux shows the old rows above new blank ones until the agent redraws.
        let deadline = Instant::now() + REDRAW;
        loop {
            std::thread::sleep(Duration::from_millis(50));
            let Ok(next) = capture(record) else {
                return screen;
            };
            let drawn = complete(&next) || clipped(&next) && blank_below(&next, height) <= gap + 1;
            screen = next;
            if drawn {
                break;
            }
            if Instant::now() >= deadline {
                return screen;
            }
        }
    }
    screen
}

/// Whether terminal rows show `expected` as text the TUI wrapped to its
/// width. A row boundary stands for one space or splits a word longer than
/// its row; every visible character must still match in order.
pub(super) fn wrapped<S: AsRef<str>>(rows: &[S], expected: &str) -> bool {
    let expected = normalize(expected);
    let mut remaining = expected.as_str();
    let rows = rows.iter().map(|row| normalize(row.as_ref()));
    for (index, row) in rows.filter(|row| !row.is_empty()).enumerate() {
        if index > 0 {
            remaining = remaining.strip_prefix(' ').unwrap_or(remaining);
        }
        let Some(rest) = remaining.strip_prefix(row.as_str()) else {
            return false;
        };
        remaining = rest;
    }
    remaining.is_empty()
}

// capture-pane -e preserves SGR styling and OSC 8 hyperlinks. Strip only this
// presentation metadata, preserving the visible label for identity checks.
// Reject other controls rather than interpreting them as application state.
pub(super) fn plain(text: &str) -> Result<String> {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            let kind = chars.next();
            if kind == Some(']') {
                if chars.next() != Some('8') || chars.next() != Some(';') {
                    return Err("unrecognized terminal escape".into());
                }
                let mut separator = false;
                let mut length = 0;
                loop {
                    match chars.next() {
                        Some('\x07') => break,
                        Some('\x1b') if chars.next() == Some('\\') => break,
                        Some(ch) if !ch.is_control() => {
                            separator |= ch == ';';
                            length += ch.len_utf8();
                            if length > 8192 {
                                return Err("terminal hyperlink is too large".into());
                            }
                        }
                        _ => return Err("incomplete or invalid terminal hyperlink".into()),
                    }
                }
                if !separator {
                    return Err("invalid terminal hyperlink".into());
                }
                continue;
            }
            if kind != Some('[') {
                return Err("unrecognized terminal escape".into());
            }
            let mut complete = false;
            for ch in chars.by_ref().take(128) {
                if ch == 'm' {
                    complete = true;
                    break;
                }
                if !(ch.is_ascii_digit() || ch == ';' || ch == ':') {
                    return Err("unrecognized terminal styling".into());
                }
            }
            if !complete {
                return Err("incomplete terminal styling".into());
            }
        } else if ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t' {
            return Err("unexpected terminal control character".into());
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum Panel {
    Question {
        index: usize,
        editing: bool,
        lines: Vec<String>,
    },
    Review {
        answers: Vec<String>,
    },
}

fn questions(card: &Value) -> Result<&Vec<Value>> {
    let items = card["questions"]
        .as_array()
        .ok_or("missing question schema")?;
    if items.is_empty() || items.len() > 12 {
        return Err("unsupported question count; answer in Terminal".into());
    }
    for item in items {
        let options = item["options"]
            .as_array()
            .ok_or("missing question options")?;
        let max = if item["multi_select"].as_bool().unwrap_or(false) {
            5
        } else {
            8
        };
        if options.is_empty() || options.len() > max {
            return Err("question has more options than can be verified in Terminal".into());
        }
    }
    Ok(items)
}

fn rule(row: &str) -> bool {
    let row = row.trim();
    row.chars().count() > 3 && row.chars().all(|ch| ch == '─')
}

/// Rows of a Kimi question dialog's bottom: its controls, every variant of
/// which ends in "esc cancel", directly above its closing rule.
fn dialog_bottom(rows: &[&str]) -> Option<usize> {
    let end = rows.iter().rposition(|row| rule(row))?;
    let controls = rows[..end].iter().rposition(|row| !row.trim().is_empty())?;
    rows[controls].contains("esc cancel").then_some(end)
}

/// A Kimi question dialog taller than its pane: its bottom shows while the
/// " question" heading at its top has scrolled off.
pub(super) fn clipped(screen: &str) -> bool {
    let Ok(plain) = plain(screen) else {
        return false;
    };
    let rows: Vec<&str> = plain.lines().collect();
    dialog_bottom(&rows).is_some_and(|end| !rows[..end].iter().any(|row| row.trim() == "question"))
}

/// A Kimi question dialog whose heading and bottom both show.
pub(super) fn complete(screen: &str) -> bool {
    let Ok(plain) = plain(screen) else {
        return false;
    };
    let rows: Vec<&str> = plain.lines().collect();
    dialog_bottom(&rows).is_some_and(|end| rows[..end].iter().any(|row| row.trim() == "question"))
}

/// Grow the pane no client views until this recorded Kimi question's clipped
/// dialog shows completely (`fit_unviewed`); the exact match in `available`
/// still decides. Only a question listing calls it.
pub(super) fn fit(record: &Value, card: &Value) {
    if string(record, "agent") != "kimi"
        || string(card, "source") != "kimi_wire"
        || record["active_tools"]
            .get(string(card, "tool_call_id"))
            .is_none_or(|tool| string(tool, "name") != "AskUserQuestion")
    {
        return;
    }
    let Ok(current) = Native::new(record, card).identity() else {
        return;
    };
    if let Ok(screen) = Native::capture(&current) {
        fit_unviewed(&current, screen, Native::capture, clipped, complete);
    }
}

fn parse_panel(screen: &str, card: &Value) -> Result<Panel> {
    let items = questions(card)?;
    if clipped(screen) {
        return Err("the question dialog is clipped; enlarge Terminal".into());
    }
    let plain = plain(screen)?;
    let rows: Vec<String> = plain.lines().map(str::to_owned).collect();
    let start = rows
        .iter()
        .rposition(|row| row.trim() == "question")
        .ok_or("the native question dialog is not visible; open Terminal")?;
    let end = rows
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, row)| rule(row))
        .map(|(index, _)| index)
        .ok_or("the question dialog is clipped; enlarge Terminal")?;
    // The TUI container adds a left margin around the dialog. The dialog's
    // own heading is exactly one leading space followed by "question".
    let margin = rows[start].len() - rows[start].trim_start().len();
    let margin = margin
        .checked_sub(1)
        .filter(|margin| *margin <= 8)
        .ok_or("unrecognized question panel margin")?;
    let lines: Vec<String> = rows[start + 1..end]
        .iter()
        .map(|row| {
            if row.trim().is_empty() {
                Ok(String::new())
            } else {
                row.strip_prefix(&" ".repeat(margin))
                    .map(str::to_owned)
                    .ok_or("inconsistent question panel margin")
            }
        })
        .collect::<std::result::Result<_, _>>()?;
    if lines.iter().any(|row| row.trim() == REVIEW) {
        if !lines.iter().any(|row| row.trim() == READY)
            || !lines.iter().any(|row| row.trim() == "→ [1] Submit")
            || !lines.iter().any(|row| row.trim() == "[2] Cancel")
            || !lines
                .iter()
                .any(|row| row.contains("1/2 choose") && row.contains("↵ confirm"))
        {
            return Err("native question review is not ready to submit".into());
        }
        let first = lines.iter().position(|row| row.trim() == REVIEW).unwrap() + 1;
        let last = lines.iter().position(|row| row.trim() == READY).unwrap();
        let mut seen = Vec::<(String, String)>::new();
        let mut in_answer = false;
        for row in &lines[first..last] {
            if let Some(text) = row.strip_prefix("  Q  ") {
                seen.push((text.to_owned(), String::new()));
                in_answer = false;
            } else if let Some(text) = row.strip_prefix("  →  ") {
                seen.last_mut().ok_or("invalid review answer")?.1 = text.to_owned();
                in_answer = true;
            } else if let Some(text) = row.strip_prefix("       ") {
                let entry = seen.last_mut().ok_or("invalid wrapped review")?;
                let field = if in_answer {
                    &mut entry.1
                } else {
                    &mut entry.0
                };
                field.push(' ');
                field.push_str(text);
            } else if !row.trim().is_empty() && row.trim() != "Some questions are still unanswered."
            {
                return Err("unrecognized native question review".into());
            }
        }
        if seen.len() != items.len()
            || seen
                .iter()
                .zip(items)
                .any(|((text, _), item)| normalize(text) != normalize(string(item, "question")))
        {
            return Err("native review does not match the pending questions".into());
        }
        return Ok(Panel::Review {
            answers: seen
                .into_iter()
                .map(|(_, answer)| normalize(&answer))
                .collect(),
        });
    }
    let question_start = lines
        .iter()
        .position(|row| row.starts_with(" ? "))
        .ok_or("unrecognized native question heading")?;
    let mut text = lines[question_start][3..].to_owned();
    for row in &lines[question_start + 1..] {
        if row.trim().is_empty() || row.trim() == EDITING {
            break;
        }
        if let Some(rest) = row.strip_prefix("   ") {
            text.push(' ');
            text.push_str(rest);
        } else {
            return Err("unrecognized wrapped question".into());
        }
    }
    let matching: Vec<_> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| normalize(string(item, "question")) == normalize(&text))
        .collect();
    if matching.len() != 1 {
        return Err("visible question does not match the pending request".into());
    }
    let editing = lines.iter().any(|row| row.trim() == EDITING);
    let hints = if editing {
        ["type answer", "↵ save", "esc cancel"]
    } else {
        ["↑↓ select", "←/→/tab switch", "esc cancel"]
    };
    if !lines
        .iter()
        .any(|row| hints.iter().all(|hint| row.contains(hint)))
    {
        return Err("unrecognized native question controls".into());
    }
    Ok(Panel::Question {
        index: matching[0].0,
        editing,
        lines,
    })
}

fn option_label(item: &Value, index: usize) -> &str {
    string(&item["options"][index], "label")
}
fn other_label(item: &Value) -> &str {
    let label = string(item, "other_label");
    if label.is_empty() {
        "Other"
    } else {
        label
    }
}

fn other_text(lines: &[String], item: &Value) -> Result<String> {
    let count = item["options"].as_array().ok_or("missing options")?.len();
    let candidates: Vec<_> = lines
        .iter()
        .filter_map(|row| {
            let text = row.trim_start();
            let rest = if item["multi_select"].as_bool().unwrap_or(false) {
                text.strip_prefix("[ ] ")
                    .or_else(|| text.strip_prefix("[✓] "))?
            } else {
                text.strip_prefix("→ ")
                    .unwrap_or(text)
                    .strip_prefix(&format!("[{}] ", count + 1))?
            };
            let rest = rest.trim_end();
            if rest == other_label(item) {
                Some(String::new())
            } else {
                rest.strip_prefix(&format!("{}:", other_label(item)))
                    .map(|text| text.strip_prefix(' ').unwrap_or(text).to_owned())
            }
        })
        .collect();
    if candidates.len() != 1 {
        return Err("Other field is not fully visible; answer in Terminal".into());
    }
    Ok(candidates[0].trim_end().to_owned())
}

fn selected_multi(lines: &[String], item: &Value) -> Result<BTreeSet<usize>> {
    let options = item["options"].as_array().ok_or("missing options")?;
    let mut selected = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for row in lines {
        let text = row.trim_start();
        let (checked, label) = if let Some(label) = text.strip_prefix("[✓] ") {
            (true, label)
        } else if let Some(label) = text.strip_prefix("[ ] ") {
            (false, label)
        } else {
            continue;
        };
        let label = label.trim_end();
        if label == other_label(item) || label.starts_with(&format!("{}: ", other_label(item))) {
            if checked {
                selected.insert(options.len());
            }
            continue;
        }
        let found: Vec<_> = options
            .iter()
            .enumerate()
            .filter(|(_, option)| string(option, "label") == label)
            .collect();
        if found.len() != 1 {
            return Err("multi-select options are wrapped or ambiguous; use Terminal".into());
        }
        let index = found[0].0;
        if !seen.insert(index) {
            return Err("duplicate visible option".into());
        }
        if checked {
            selected.insert(index);
        }
    }
    if seen.len() != options.len() {
        return Err("not all multi-select choices are visible; enlarge Terminal".into());
    }
    Ok(selected)
}

#[derive(Clone)]
struct Desired {
    selected: BTreeSet<usize>,
    text: String,
    answer: String,
}

fn desired_answers(card: &Value, answers: &Value) -> Result<Vec<Desired>> {
    let items = questions(card)?;
    let answers = answers.as_array().ok_or("answers must be an array")?;
    if answers.len() != items.len() {
        return Err("answer every question before submitting".into());
    }
    items
        .iter()
        .map(|item| {
            let matches: Vec<_> = answers
                .iter()
                .filter(|answer| string(answer, "question_id") == string(item, "id"))
                .collect();
            if matches.len() != 1 {
                return Err("missing or duplicate question answer".into());
            }
            let answer = matches[0];
            let options = item["options"].as_array().unwrap();
            let chosen = answer["selected_option_ids"]
                .as_array()
                .ok_or("selected_option_ids must be an array")?;
            let mut selected = BTreeSet::new();
            for id in chosen {
                let id = id.as_str().ok_or("invalid option id")?;
                let index = options
                    .iter()
                    .position(|option| string(option, "id") == id)
                    .ok_or("unknown option id")?;
                if !selected.insert(index) {
                    return Err("duplicate selected option".into());
                }
            }
            let text = string(answer, "text").trim().to_owned();
            if text.len() > 4096
                || text.chars().any(char::is_control)
                || text.starts_with(['/', '!'])
            {
                return Err(
                    "Other must be one line of plain text; use Terminal for commands".into(),
                );
            }
            if (!text.is_empty() && item["allow_other"] == false)
                || (selected.is_empty() && text.is_empty())
                || (!item["multi_select"].as_bool().unwrap_or(false)
                    && selected.len() + usize::from(!text.is_empty()) != 1)
            {
                return Err("invalid selection for question".into());
            }
            let mut labels: Vec<_> = selected
                .iter()
                .map(|index| option_label(item, *index).to_owned())
                .collect();
            if !text.is_empty() {
                labels.push(text.clone());
            }
            Ok(Desired {
                selected,
                text,
                answer: labels.join(", "),
            })
        })
        .collect()
}

pub(super) trait Transport {
    fn screen(&mut self) -> Result<String>;
    fn key(&mut self, key: &str) -> Result<()>;
    fn paste(&mut self, text: &str) -> Result<()>;
    fn resolved(&mut self) -> Result<Option<Value>>;
    fn pause(&mut self) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_panel<T: Transport>(
    transport: &mut T,
    card: &Value,
    expected: impl Fn(&Panel) -> bool,
) -> Result<Panel> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let panel = parse_panel(&transport.screen()?, card)?;
        if expected(&panel) {
            return Ok(panel);
        }
        if Instant::now() >= deadline {
            return Err("native question did not reach the expected state".into());
        }
        transport.pause();
    }
}

fn goto_question<T: Transport>(transport: &mut T, card: &Value, index: usize) -> Result<Panel> {
    for _ in 0..=questions(card)?.len() {
        let panel = parse_panel(&transport.screen()?, card)?;
        if let Panel::Question {
            index: current,
            editing,
            ..
        } = &panel
        {
            if *editing {
                return Err("Other already has an active draft; finish it in Terminal".into());
            }
            if *current == index {
                return Ok(panel);
            }
        }
        advance_tab(transport, card, &panel)?;
    }
    Err("could not select the requested question tab".into())
}

fn advance_tab<T: Transport>(transport: &mut T, card: &Value, panel: &Panel) -> Result<()> {
    let next = match panel {
        Panel::Question {
            index,
            editing: false,
            ..
        } => index + 1,
        Panel::Review { .. } => 0,
        _ => return Err("cannot navigate away from an active Other draft".into()),
    };
    let count = questions(card)?.len();
    transport.key("Tab")?;
    wait_panel(transport, card, |panel| match panel {
        Panel::Question {
            index,
            editing: false,
            ..
        } => *index == next,
        Panel::Review { .. } => next == count,
        _ => false,
    })?;
    Ok(())
}

fn drive<T: Transport>(transport: &mut T, card: &Value, answers: &Value) -> Result<()> {
    let wanted = desired_answers(card, answers)?;
    let items = questions(card)?;
    let expected: Vec<_> = wanted
        .iter()
        .map(|answer| normalize(&answer.answer))
        .collect();
    // An explicit retry may find the previous attempt in its native review or
    // the single question's Other editor. Saving an existing editor preserves
    // its text; only the full review can authorize final submission. Never
    // paste again or replace a draft whose delivery was uncertain.
    let resume_review = match parse_panel(&transport.screen()?, card)? {
        Panel::Review { answers } => answers == expected,
        Panel::Question {
            index: 0,
            editing: true,
            ..
        } if items.len() == 1 => {
            if wanted[0].text.is_empty() || !wanted[0].selected.is_empty() {
                return Err(
                    "finish the active Other draft before choosing a different answer".into(),
                );
            }
            transport.key("Enter")?;
            wait_panel(transport, card, |panel| {
                !matches!(panel, Panel::Question { editing: true, .. })
            })?;
            true
        }
        _ => false,
    };
    if !resume_review {
        for (index, (item, desired)) in items.iter().zip(&wanted).enumerate() {
            let panel = goto_question(transport, card, index)?;
            let Panel::Question { lines, .. } = panel else {
                unreachable!()
            };
            if item["multi_select"].as_bool().unwrap_or(false) {
                let target = desired.selected.clone();
                // Other is committed after preset toggles, so first remove any old
                // selection rather than attributing an old draft to the new answer.
                let selected = selected_multi(&lines, item)?;
                if !desired.text.is_empty()
                    && selected.contains(&item["options"].as_array().unwrap().len())
                {
                    return Err("Other already has a selected draft; finish it in Terminal".into());
                }
                for choice in selected
                    .symmetric_difference(&target)
                    .copied()
                    .collect::<Vec<_>>()
                {
                    let panel = parse_panel(&transport.screen()?, card)?;
                    let Panel::Question {
                        index: current,
                        editing: false,
                        lines,
                    } = panel
                    else {
                        return Err("question changed while selecting options".into());
                    };
                    if current != index {
                        return Err("active question changed".into());
                    }
                    let before = selected_multi(&lines, item)?;
                    let should_select = target.contains(&choice);
                    if before.contains(&choice) == should_select {
                        continue;
                    }
                    transport.key(&(choice + 1).to_string())?;
                    wait_panel(transport, card, |panel| match panel {
                        Panel::Question {
                            index: current,
                            editing: false,
                            lines,
                        } if *current == index => selected_multi(lines, item)
                            .is_ok_and(|current| current.contains(&choice) == should_select),
                        _ => false,
                    })?;
                }
            } else if let Some(choice) = desired.selected.iter().next() {
                transport.key(&(choice + 1).to_string())?;
                wait_panel(
                    transport,
                    card,
                    |panel| !matches!(panel, Panel::Question { index: current, .. } if *current == index),
                )?;
            }
            if !desired.text.is_empty() {
                let panel = goto_question(transport, card, index)?;
                let Panel::Question { lines, .. } = panel else {
                    unreachable!()
                };
                if !other_text(&lines, item)?.is_empty() {
                    return Err("Other has an existing draft; finish it in Terminal".into());
                }
                transport.key(&(item["options"].as_array().unwrap().len() + 1).to_string())?;
                let panel = wait_panel(
                    transport,
                    card,
                    |panel| matches!(panel, Panel::Question { index: current, editing: true, .. } if *current == index),
                )?;
                let Panel::Question { lines, .. } = panel else {
                    unreachable!()
                };
                if !other_text(&lines, item)?.is_empty() {
                    return Err("Other changed before paste".into());
                }
                transport.paste(&desired.text)?;
                wait_panel(transport, card, |panel| match panel {
                    Panel::Question {
                        index: current,
                        editing: true,
                        lines,
                    } if *current == index => {
                        // Kimi scrolls a long one-line editor horizontally. Its
                        // visible suffix cannot prove the whole draft. Enter here
                        // saves only this exact editor; the full wrapped review
                        // below must still match every requested answer.
                        other_text(lines, item).is_ok_and(|text| !text.is_empty())
                    }
                    _ => false,
                })?;
                // A single Enter is used only in a verified, exact Other field.
                // Final submission uses the dialog's explicit numeric action.
                transport.key("Enter")?;
                wait_panel(transport, card, |panel| {
                    !matches!(panel, Panel::Question { editing: true, .. })
                })?;
            }
        }
    }
    let mut review = None;
    for _ in 0..=items.len() {
        let panel = parse_panel(&transport.screen()?, card)?;
        if let Panel::Review { answers } = panel {
            review = Some(answers);
            break;
        }
        if matches!(panel, Panel::Question { editing: true, .. }) {
            return Err("Other draft was not committed".into());
        }
        advance_tab(transport, card, &panel)?;
    }
    if review.as_ref() != Some(&expected) {
        return Err("native review differs from your chosen answers; inspect Terminal".into());
    }
    // Re-capture immediately before the sole final submit action.
    if !matches!(parse_panel(&transport.screen()?, card)?, Panel::Review { answers } if answers == expected)
    {
        return Err("native review changed before submission".into());
    }
    transport.key("1")?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(response) = transport.resolved()? {
            let returned = response["answers"]
                .as_object()
                .ok_or("question was dismissed instead of answered")?;
            if returned.len() != items.len()
                || items.iter().zip(&wanted).any(|(item, desired)| {
                    returned
                        .get(string(item, "question"))
                        .and_then(Value::as_str)
                        != Some(desired.answer.as_str())
                })
            {
                return Err(
                    "native question acknowledgement differs from submitted answers".into(),
                );
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("native question acknowledgement was not observed".into());
        }
        transport.pause();
    }
}

pub(super) struct Native<'a> {
    original: &'a Value,
    card: &'a Value,
    touched: bool,
    last_screen: Option<String>,
}
impl Native<'_> {
    pub(super) fn new<'a>(record: &'a Value, card: &'a Value) -> Native<'a> {
        Native {
            original: record,
            card,
            touched: false,
            last_screen: None,
        }
    }
    pub(super) fn touched(&self) -> bool {
        self.touched
    }
    pub(super) fn identity(&self) -> Result<Value> {
        let current = read_run(
            string(self.original, "name"),
            string(self.original, "run_id"),
        )?
        .ok_or("session run disappeared")?;
        if ["pane", "pid", "process_start", "conversation_id"]
            .iter()
            .any(|key| current[*key] != self.original[*key])
            || !matches(&current, panes_of(&live()?, &current))
            || !process_alive(&current)
            || pause_active(&current)
            || !string(&current, "error").is_empty()
        {
            return Err("session identity changed; open Terminal".into());
        }
        input::checked_terminal(&current, false)?;
        Ok(current)
    }
    fn pending(&self) -> Result<Value> {
        let current = self.identity()?;
        let pending = super::questions::current(&current)?;
        if !pending.iter().any(|card| {
            string(card, "question_id") == string(self.card, "question_id")
                && string(card, "tool_call_id") == string(self.card, "tool_call_id")
                && card["questions"] == self.card["questions"]
        }) {
            return Err("the pending question changed or was already answered".into());
        }
        Ok(current)
    }
    pub(super) fn capture(current: &Value) -> Result<String> {
        let result =
            backend::current().capture(string(current, "pane"), backend::Capture::Styled)?;
        if result.len() > 512 * 1024 {
            return Err("terminal snapshot is too large".into());
        }
        String::from_utf8(result).map_err(|_| "terminal snapshot is not UTF-8".into())
    }
    fn before_write(&self) -> Result<Value> {
        let current = self.pending()?;
        let actual = Self::capture(&current)?;
        let unchanged = self.last_screen.as_ref().is_some_and(|previous| {
            if string(&current, "agent") == "claude" {
                // Claude repaints surrounding transcript/status while its
                // question waits. Verify the complete question and selections,
                // not unrelated animation or ANSI color changes above it.
                super::claude_question::same_panel(previous, &actual, self.card)
            } else if string(self.card, "source") == "codex_async" {
                super::codex_questions::same_panel(previous, &actual, self.card)
            } else if string(&current, "agent") == "kimi" && super::kimi_goal::is(self.card) {
                super::kimi_approval::bound_goal(&current, self.card).is_ok_and(|event| {
                    super::kimi_goal::same_panel(previous, &actual, &event["request"])
                })
            } else {
                previous == &actual
            }
        });
        if !unchanged {
            return Err(
                "terminal changed while preparing the answer; inspect it before retrying".into(),
            );
        }
        Ok(current)
    }
}
impl Transport for Native<'_> {
    fn screen(&mut self) -> Result<String> {
        let current = self.pending()?;
        let screen = Self::capture(&current)?;
        self.last_screen = Some(screen.clone());
        Ok(screen)
    }
    fn key(&mut self, key: &str) -> Result<()> {
        let current = self.before_write()?;
        self.touched = true;
        backend::current().send(string(&current, "pane"), &[backend::Key::Named(key)])
    }
    fn paste(&mut self, text: &str) -> Result<()> {
        let buffer = format!("hgs-question-{}", uuid::Uuid::new_v4());
        let terminal = backend::current();
        let loaded = terminal.stage(&buffer, text.as_bytes(), backend::StageErrors::Discard)?;
        let result = (|| {
            if !loaded {
                return Err("failed to stage Other text".into());
            }
            let current = self.before_write()?;
            self.touched = true;
            terminal.paste(&buffer, string(&current, "pane"), backend::Paste::Composer)
        })();
        terminal.discard(&buffer);
        result
    }
    fn resolved(&mut self) -> Result<Option<Value>> {
        let current = self.identity()?;
        Ok(super::questions::response(&current, self.card)?.map(|event| event["response"].clone()))
    }
}

pub(super) fn available(record: &Value, card: &Value) -> Result<()> {
    if string(record, "agent") != "kimi" {
        return Err("this provider requires answering in Terminal".into());
    }
    let native = Native {
        original: record,
        card,
        touched: false,
        last_screen: None,
    };
    let current = native.identity()?;
    let panel = parse_panel(&Native::capture(&current)?, card)?;
    if matches!(panel, Panel::Question { editing: true, .. }) && questions(card)?.len() != 1 {
        return Err("finish the active Other draft in Terminal".into());
    }
    Ok(())
}

pub(super) fn answer(record: &Value, card: &Value, answers: &Value) -> Result<()> {
    available(record, card)?;
    desired_answers(card, answers)?;
    let mut native = Native {
        original: record,
        card,
        touched: false,
        last_screen: None,
    };
    drive(&mut native, card, answers).map_err(|error| {
        if native.touched {
            format!("delivery uncertain: {error}; inspect Terminal and do not resend automatically")
        } else {
            error
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_blank_rows_do_not_count_as_drawn() {
        let screen = "\x1b[38;5;246m❯ 1. Yes\x1b[39m\n\x1b[0m\n\x1b[2m   \x1b[0m\n\x1b]8;;\x1b\\\n";
        assert_eq!(blank_below(screen, 24), 23);
        assert_eq!(blank_below("", 24), 24);
        assert_eq!(blank_below("x\n", 0), 0);
    }

    #[test]
    fn plain_preserves_visible_hyperlink_text_and_styling() {
        for terminator in ["\x1b\\", "\x07"] {
            let screen = format!(
                "Before \x1b]8;id=fixture;https://example.invalid/artifact{terminator}\x1b[1;34mArtifact 界\x1b[0m\x1b]8;;{terminator}\nAfter"
            );
            assert_eq!(plain(&screen).unwrap(), "Before Artifact 界\nAfter");
        }
    }

    #[test]
    fn plain_rejects_other_escapes_and_malformed_hyperlinks() {
        for screen in [
            "\x1b]52;c;data\x07",
            "\x1b]0;title\x1b\\",
            "\x1bPdata\x1b\\",
            "\x1b[2J",
            "\x1b]8;;https://example.invalid/unfinished",
            "\x1b]8;missing-delimiter\x1b\\",
            "\x1b]8;;https://example.invalid/\nspoofed\x1b\\",
            "\x1b]8;;https://example.invalid/\x1b[0m\x1b\\",
            "\x1b]8;;https://example.invalid/\x00\x07",
        ] {
            assert!(plain(screen).is_err(), "accepted {screen:?}");
        }
        assert!(plain(&format!("\x1b]8;;{}\x1b\\", "x".repeat(8193))).is_err());
    }

    fn card() -> Value {
        json!({"question_id":"question_fixture", "tool_call_id":"tool_fixture", "questions":[
            {"id":"q_0","question":"Which approach?","header":"Approach","multi_select":false,"allow_other":true,
                "options":[{"id":"opt_0_0","label":"Incremental"},{"id":"opt_0_1","label":"Complete"}]},
            {"id":"q_1","question":"Which checks?","header":"Checks","multi_select":true,"allow_other":true,
                "options":[{"id":"opt_1_0","label":"Linux"},{"id":"opt_1_1","label":"macOS"}]},
            {"id":"q_2","question":"Any further detail?","header":"Detail","multi_select":false,"allow_other":true,
                "options":[{"id":"opt_2_0","label":"None"},{"id":"opt_2_1","label":"Documentation"}]}
        ]})
    }
    fn answers() -> Value {
        json!([
            {"question_id":"q_0","selected_option_ids":["opt_0_1"],"text":""},
            {"question_id":"q_1","selected_option_ids":["opt_1_0","opt_1_1"],"text":""},
            {"question_id":"q_2","selected_option_ids":[],"text":"Проверь Unicode 界"}
        ])
    }

    // Faithful isolated model of the installed Kimi 2.1.1 QuestionDialog:
    // numeric single choices advance to the next unanswered tab; multiselect
    // toggles; Other commits on Enter; the review's numeric1 emits its answers.
    // No actual agent, terminal, clipboard or model request is involved.
    struct Dialog {
        card: Value,
        tab: usize,
        selected: Vec<BTreeSet<usize>>,
        drafts: Vec<String>,
        committed: Vec<String>,
        editing: bool,
        keys: Vec<String>,
        response: Option<Value>,
        corrupt_review: bool,
        disappear_after_input: bool,
        clip_other: bool,
    }
    impl Dialog {
        fn new() -> Self {
            Self {
                card: card(),
                tab: 0,
                selected: vec![BTreeSet::new(); 3],
                drafts: vec![String::new(); 3],
                committed: vec![String::new(); 3],
                editing: false,
                keys: vec![],
                response: None,
                corrupt_review: false,
                disappear_after_input: false,
                clip_other: false,
            }
        }
        fn answer(&self, index: usize) -> String {
            let item = &self.card["questions"][index];
            let mut labels: Vec<_> = self.selected[index]
                .iter()
                .map(|choice| {
                    if *choice == 2 {
                        self.committed[index].clone()
                    } else {
                        option_label(item, *choice).to_owned()
                    }
                })
                .collect();
            labels.retain(|label| !label.is_empty());
            labels.join(", ")
        }
        fn advance(&mut self) {
            self.tab = ((self.tab + 1)..self.card["questions"].as_array().unwrap().len())
                .find(|index| self.answer(*index).is_empty())
                .unwrap_or(self.card["questions"].as_array().unwrap().len());
        }
        fn rendered(&self) -> String {
            if self.disappear_after_input && !self.keys.is_empty() {
                return "│ > draft\n╰─────────────╯".into();
            }
            let mut rows = vec![
                "────────────────────────────────────────────────".to_owned(),
                " question".into(),
                String::new(),
                " Approach  Checks  Detail  Submit".into(),
                String::new(),
            ];
            if self.tab == self.card["questions"].as_array().unwrap().len() {
                rows.push(format!(" {REVIEW}"));
                rows.push(String::new());
                for index in 0..self.card["questions"].as_array().unwrap().len() {
                    rows.push(format!(
                        "  Q  {}",
                        string(&self.card["questions"][index], "question")
                    ));
                    let answer = if self.corrupt_review {
                        "someone else's answer".into()
                    } else {
                        self.answer(index)
                    };
                    rows.push(format!(
                        "  →  {}",
                        if answer.is_empty() {
                            "Not answered"
                        } else {
                            &answer
                        }
                    ));
                }
                rows.extend([
                    String::new(),
                    format!(" {READY}"),
                    String::new(),
                    "  → [1] Submit".into(),
                    "    [2] Cancel".into(),
                    String::new(),
                    "  ↑↓ select  1/2 choose  ↵ confirm  ←/→/tab switch  esc cancel".into(),
                ]);
            } else {
                let item = &self.card["questions"][self.tab];
                rows.push(format!(" ? {}", string(item, "question")));
                if self.editing {
                    rows.push(format!("   {EDITING}"));
                }
                rows.push(String::new());
                for choice in 0..3 {
                    let mut label = if choice == 2 {
                        "Other".into()
                    } else {
                        option_label(item, choice).to_owned()
                    };
                    if choice == 2 && (self.editing || !self.drafts[self.tab].is_empty()) {
                        let shown = if self.editing && self.clip_other {
                            self.drafts[self.tab].chars().skip(1).collect::<String>()
                        } else {
                            self.drafts[self.tab].clone()
                        };
                        label += &format!(": {shown}");
                    }
                    if self.tab == 1 {
                        rows.push(format!(
                            "  [{}] {label}",
                            if self.selected[self.tab].contains(&choice) {
                                "✓"
                            } else {
                                " "
                            }
                        ));
                    } else {
                        rows.push(format!("  → [{}] {label}", choice + 1));
                    }
                }
                rows.push(String::new());
                rows.push(if self.editing {
                    "  type answer  ↵ save  tab switch  esc cancel".into()
                } else {
                    "  ↑↓ select  1-3 / ↵ choose  ←/→/tab switch  esc cancel".into()
                });
            }
            rows.push("────────────────────────────────────────────────".into());
            // Actual Kimi has a one-column container margin and SGR styling.
            rows.into_iter()
                .map(|row| format!(" \x1b[38;2;90;170;220m{row}\x1b[0m"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }
    impl Transport for Dialog {
        fn screen(&mut self) -> Result<String> {
            Ok(self.rendered())
        }
        fn key(&mut self, key: &str) -> Result<()> {
            self.keys.push(key.into());
            if key == "Tab" {
                self.tab = (self.tab + 1) % (self.card["questions"].as_array().unwrap().len() + 1);
                self.editing = false;
                return Ok(());
            }
            if self.tab == self.card["questions"].as_array().unwrap().len() {
                if key != "1" {
                    return Err("unexpected submit action".into());
                }
                let mut answers = serde_json::Map::new();
                for index in 0..self.card["questions"].as_array().unwrap().len() {
                    answers.insert(
                        string(&self.card["questions"][index], "question").into(),
                        json!(self.answer(index)),
                    );
                }
                self.response = Some(json!({"answers":answers,"method":"number_key"}));
                return Ok(());
            }
            if self.editing {
                if key != "Enter" {
                    return Err("unexpected Other action".into());
                }
                self.committed[self.tab] = self.drafts[self.tab].clone();
                self.selected[self.tab].insert(2);
                self.editing = false;
                if self.tab != 1 {
                    self.advance();
                }
                return Ok(());
            }
            let choice = key
                .parse::<usize>()
                .map_err(|_| "unexpected numeric choice")?
                - 1;
            if choice > 2 {
                return Err("invalid numeric choice".into());
            }
            if choice == 2 {
                self.editing = true;
                return Ok(());
            }
            if self.tab == 1 {
                if !self.selected[self.tab].remove(&choice) {
                    self.selected[self.tab].insert(choice);
                }
            } else {
                self.selected[self.tab].clear();
                self.selected[self.tab].insert(choice);
                self.advance();
            }
            Ok(())
        }
        fn paste(&mut self, text: &str) -> Result<()> {
            if !self.editing {
                return Err("paste outside Other".into());
            }
            self.keys.push("paste".into());
            self.drafts[self.tab].push_str(text);
            Ok(())
        }
        fn resolved(&mut self) -> Result<Option<Value>> {
            Ok(self.response.clone())
        }
        fn pause(&mut self) {}
    }

    #[test]
    fn exact_native_dialog_roundtrip_including_other_and_multiselect() {
        let mut dialog = Dialog::new();
        drive(&mut dialog, &card(), &answers()).unwrap();
        assert_eq!(dialog.answer(0), "Complete");
        assert_eq!(dialog.answer(1), "Linux, macOS");
        assert_eq!(dialog.answer(2), "Проверь Unicode 界");
        assert_eq!(
            dialog
                .keys
                .iter()
                .filter(|key| key.as_str() == "Enter")
                .count(),
            1
        );
        assert_eq!(dialog.keys.last().unwrap(), "1");
        assert!(dialog.response.is_some());
    }
    #[test]
    fn rendered_installed_kimi_widget_and_input_roundtrip() {
        struct Golden {
            data: Value,
            step: usize,
        }
        impl Golden {
            fn act(&mut self, kind: &str, value: &str) -> Result<()> {
                let next = &self.data["frames"][self.step + 1];
                if next["kind"] != kind || next["value"] != value {
                    return Err(format!("unexpected native widget action: {kind} {value}"));
                }
                self.step += 1;
                Ok(())
            }
        }
        impl Transport for Golden {
            fn screen(&mut self) -> Result<String> {
                self.data["frames"][self.step]["screen"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or("native widget is no longer displayed".into())
            }
            fn key(&mut self, key: &str) -> Result<()> {
                self.act("key", key)
            }
            fn paste(&mut self, text: &str) -> Result<()> {
                self.act("paste", text)
            }
            fn resolved(&mut self) -> Result<Option<Value>> {
                Ok(
                    (self.step + 1 == self.data["frames"].as_array().unwrap().len())
                        .then(|| self.data["response"].clone()),
                )
            }
            fn pause(&mut self) {}
        }
        let mut native = Golden {
            data: serde_json::from_str(include_str!("fixtures/kimi_question_2_1_1.json")).unwrap(),
            step: 0,
        };
        drive(&mut native, &card(), &answers()).unwrap();
        assert_eq!(native.step, 8);
    }
    #[test]
    fn checks_existing_multiselect_state_instead_of_blind_toggles() {
        let mut dialog = Dialog::new();
        dialog.selected[1].insert(0);
        drive(&mut dialog, &card(), &answers()).unwrap();
        assert_eq!(dialog.answer(1), "Linux, macOS");
    }
    #[test]
    fn never_submits_mismatched_review() {
        let mut dialog = Dialog::new();
        dialog.corrupt_review = true;
        assert!(drive(&mut dialog, &card(), &answers())
            .unwrap_err()
            .contains("review differs"));
        assert!(dialog.response.is_none());
        assert_eq!(dialog.keys.last().unwrap(), "Enter");
    }
    #[test]
    fn disappearing_modal_stops_after_the_first_non_enter_key() {
        let mut dialog = Dialog::new();
        dialog.disappear_after_input = true;
        assert!(drive(&mut dialog, &card(), &answers()).is_err());
        assert_eq!(dialog.keys, vec!["2"]);
        assert!(dialog.response.is_none());
    }
    #[test]
    fn horizontally_scrolled_other_is_verified_in_the_full_review() {
        let mut dialog = Dialog::new();
        dialog.clip_other = true;
        drive(&mut dialog, &card(), &answers()).unwrap();
        assert_eq!(dialog.committed[2], answers()[2]["text"].as_str().unwrap());
        assert!(dialog.response.is_some());
    }

    fn single_other(draft: &str, editing: bool) -> (Dialog, Value) {
        let mut dialog = Dialog::new();
        let item = dialog.card["questions"][2].clone();
        dialog.card["questions"] = json!([item]);
        dialog.drafts[0] = draft.into();
        dialog.editing = editing;
        dialog.clip_other = true;
        (dialog, json!([answers()[2].clone()]))
    }

    #[test]
    fn explicit_retry_saves_the_existing_other_without_pasting_again() {
        let text = answers()[2]["text"].as_str().unwrap().to_owned();
        let (mut dialog, answers) = single_other(&text, true);
        let card = dialog.card.clone();
        drive(&mut dialog, &card, &answers).unwrap();
        assert_eq!(dialog.keys, vec!["Enter", "1"]);
        assert_eq!(dialog.committed[0], text);
    }

    #[test]
    fn different_existing_other_is_preserved_without_final_submission() {
        let (mut dialog, answers) = single_other("A foreign native draft", true);
        let card = dialog.card.clone();
        assert!(drive(&mut dialog, &card, &answers)
            .unwrap_err()
            .contains("review differs"));
        assert_eq!(dialog.keys, vec!["Enter"]);
        assert_eq!(dialog.committed[0], "A foreign native draft");
        assert!(dialog.response.is_none());
    }

    #[test]
    fn explicit_retry_of_the_exact_review_uses_only_its_submit_action() {
        let text = answers()[2]["text"].as_str().unwrap().to_owned();
        let (mut dialog, answers) = single_other(&text, false);
        dialog.committed[0] = text;
        dialog.selected[0].insert(2);
        dialog.tab = 1;
        let card = dialog.card.clone();
        drive(&mut dialog, &card, &answers).unwrap();
        assert_eq!(dialog.keys, vec!["1"]);
    }

    #[test]
    fn never_overwrites_existing_other_draft() {
        let mut dialog = Dialog::new();
        dialog.drafts[2] = "user draft".into();
        assert!(drive(&mut dialog, &card(), &answers())
            .unwrap_err()
            .contains("existing draft"));
        assert!(!dialog
            .keys
            .iter()
            .any(|key| key == "paste" || key == "Enter"));
        assert_eq!(dialog.drafts[2], "user draft");
    }
    #[test]
    fn question_markup_requires_exact_pending_text_and_visible_footer() {
        let dialog = Dialog::new();
        let screen = dialog.rendered();
        assert!(parse_panel(&screen, &card()).is_ok());
        assert!(parse_panel(
            &screen.replace("Which approach?", "Different question?"),
            &card()
        )
        .is_err());
        assert!(parse_panel(&screen.replace("esc cancel", "unknown action"), &card()).is_err());
        assert!(parse_panel(&screen.replace("\x1b[0m", "\x1b]52;c;data\x07"), &card()).is_err());
    }
    #[test]
    fn other_rejects_terminal_commands_and_control_input_before_any_key() {
        for text in ["first\nsecond", "\x1b[201~", "/exit", "!rm example"] {
            let mut answers = answers();
            answers[2]["text"] = json!(text);
            let mut dialog = Dialog::new();
            assert!(drive(&mut dialog, &card(), &answers).is_err());
            assert!(dialog.keys.is_empty());
        }
    }
    #[test]
    fn clipped_dialog_differs_from_complete_one() {
        // Kimi 2.1.1 and 2.2.0 at 80x24: the bottom of a tall dialog shows, its heading does not.
        let clipped_dialog = include_str!("../../tests/fixtures/kimi-question-clipped-80x24.txt");
        assert!(clipped(clipped_dialog));
        assert!(!complete(clipped_dialog));
        let option = json!({"label":"Option","description":"Любой провайдер из списка"});
        let tall = json!({"questions":[{"question":"Which release should we prepare?",
            "options":[option,option,option,option]}]});
        assert_eq!(
            parse_panel(clipped_dialog, &tall).unwrap_err(),
            "the question dialog is clipped; enlarge Terminal"
        );
        let mut dialog = Dialog::new();
        for _ in 0..2 {
            for screen in [
                dialog.rendered(),
                format!("transcript\n{}\n status", dialog.rendered()),
            ] {
                assert!(complete(&screen) && !clipped(&screen), "{screen}");
            }
            dialog.editing = true;
        }
        dialog.tab = 3;
        assert!(complete(&dialog.rendered()) && !clipped(&dialog.rendered()));
        // Without its controls row above the closing rule it is neither.
        for screen in [
            "│ > draft\n╰─────────────╯".to_owned(),
            clipped_dialog.replace("esc cancel", ""),
            " ▶ Ready to build with this plan?\n\n ▶ 1. Approve\n\n ↑/↓ select · 1/2/3 choose · ↵ confirm\n ──────".to_owned(),
        ] {
            assert!(!clipped(&screen) && !complete(&screen), "{screen}");
        }
    }
}
