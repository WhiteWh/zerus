//! Kimi CreateGoal approvals in Activity. The complete goal comes from the
//! main agent's wire; delivery uses the matching native ApprovalPanel only.
use super::question_terminal::{self, Native, Transport};
use super::*;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

const MAX_BODY_CHARS: usize = 16_000;
const NARROW: &str =
    "Kimi's approval does not show this whole request; enlarge Terminal or answer there";
const INVALID: &str = "Zerus cannot verify this CreateGoal request. Answer it in Terminal.";
// Kimi Code 2.4.1 goalStartOptions (goal-start-permission-prompt.ts), shown
// verbatim by its CreateGoal ApprovalPanel.
const AUTO_DESCRIPTION: &str = "Best if you want Kimi Code to keep working while you are away. Tools are approved automatically and questions are skipped; only plans in Plan mode wait for your review.";
const YOLO_SWITCH_DESCRIPTION: &str =
    "Tools are approved automatically. Kimi Code may still ask you questions.";
const YOLO_KEEP_DESCRIPTION: &str =
    "Tools stay approved automatically. Kimi Code may still ask you questions.";
const MANUAL_DESCRIPTION: &str = "Keep approvals on. Kimi Code will ask before risky actions, so the goal may stop and wait for you.";
const CANCEL_DESCRIPTION: &str = "Return to the input box with your goal command.";

#[derive(Clone, Copy)]
struct GoalChoice {
    id: &'static str,
    label: &'static str,
    description: &'static str,
}
const AUTO: GoalChoice = GoalChoice {
    id: "auto",
    label: "Switch to Never Ask and start",
    description: AUTO_DESCRIPTION,
};
const YOLO_SWITCH: GoalChoice = GoalChoice {
    id: "yolo",
    label: "Switch to Ask When Needed and start",
    description: YOLO_SWITCH_DESCRIPTION,
};
const YOLO_KEEP: GoalChoice = GoalChoice {
    id: "yolo",
    label: "Keep Ask When Needed and start",
    description: YOLO_KEEP_DESCRIPTION,
};
const MANUAL: GoalChoice = GoalChoice {
    id: "manual",
    label: "Start in Always Ask",
    description: MANUAL_DESCRIPTION,
};
const CANCEL: GoalChoice = GoalChoice {
    id: "cancel",
    label: "Do not start",
    description: CANCEL_DESCRIPTION,
};
const CHOICES: [GoalChoice; 5] = [AUTO, YOLO_SWITCH, YOLO_KEEP, MANUAL, CANCEL];

#[derive(Clone, Copy)]
struct Goal<'a> {
    objective: &'a str,
    criterion: Option<&'a str>,
    mode: &'a str,
}

impl Goal<'_> {
    fn body(&self) -> String {
        let mut body = format!("Start goal: {}", self.objective);
        if let Some(criterion) = self.criterion.filter(|criterion| !criterion.is_empty()) {
            body.push_str(&format!("\nDone when: {criterion}"));
        }
        body
    }

    fn choices(&self) -> Vec<GoalChoice> {
        if self.mode == "yolo" {
            vec![AUTO, YOLO_KEEP, CANCEL]
        } else {
            vec![AUTO, YOLO_SWITCH, MANUAL, CANCEL]
        }
    }

    fn options(&self) -> Value {
        self.choices()
            .into_iter()
            .map(|choice| {
                json!({"id":choice.id,"label":choice.label,"description":choice.description,
                    "scope":if choice.id != "cancel" && choice.id != self.mode {"broader"} else {"once"}})
            })
            .collect()
    }
}

pub(super) fn is(card: &Value) -> bool {
    string(card, "source") == kimi_approval::SOURCE && string(&card["questions"][0], "id") == "goal"
}

pub(super) fn recover(record: &Value, events: &[Value]) -> Vec<Value> {
    if string(record, "agent") != "kimi" {
        return Vec::new();
    }
    kimi_approval::unresolved(events)
        .iter()
        .filter_map(|event| card(record, event))
        .collect()
}

fn text(text: &str) -> bool {
    !text.trim().is_empty()
        && text
            .chars()
            .all(|ch| ch == '\n' || !ch.is_control() && ch.width().is_some_and(|width| width > 0))
}

fn goal(request: &Value) -> Result<Goal<'_>> {
    let display = &request["display"];
    let fields = display.as_object().ok_or(INVALID)?;
    if string(request, "toolName") != "CreateGoal"
        || string(display, "kind") != "goal_start"
        || fields.keys().any(|key| {
            !["kind", "objective", "completionCriterion", "mode"].contains(&key.as_str())
        })
    {
        return Err(INVALID.into());
    }
    let objective = display["objective"].as_str().ok_or(INVALID)?;
    let criterion = match display.get("completionCriterion") {
        None => None,
        Some(Value::String(criterion)) => Some(criterion.as_str()),
        _ => return Err(INVALID.into()),
    };
    let mode = string(display, "mode");
    if !text(objective)
        || criterion.is_some_and(|criterion| {
            !criterion.is_empty()
                && criterion.chars().any(|ch| {
                    ch != '\n' && (ch.is_control() || ch.width().is_none_or(|width| width == 0))
                })
        })
        || !["manual", "yolo"].contains(&mode)
    {
        return Err(INVALID.into());
    }
    Ok(Goal {
        objective,
        criterion,
        mode,
    })
}

fn literal(request: &Value) -> String {
    let display = serde_json::to_string_pretty(&request["display"]).unwrap_or_default();
    format!("{}\n\n{display}", string(request, "action"))
        .trim()
        .to_owned()
}

pub(super) fn card(record: &Value, event: &Value) -> Option<Value> {
    let id = string(event, "id");
    let call = string(event, "toolCallId");
    let request = &event["request"];
    if string(event, "type") != "interaction.request"
        || string(event, "kind") != "approval"
        || string(event, "agentId") != "main"
        || id.is_empty()
        || id.len() > 240
        || call.is_empty()
        || call.len() > 200
        || string(request, "id") != id
        || string(request, "toolCallId") != call
        || string(request, "toolName") != "CreateGoal"
        || string(&request["display"], "kind") != "goal_start"
        || !record["active_tools"]
            .get(call)
            .is_some_and(|active| string(active, "name") == "CreateGoal")
    {
        return None;
    }
    let (body, problem, options) = match goal(request) {
        Ok(goal) => (goal.body(), None, goal.options()),
        Err(problem) => (literal(request), Some(problem), json!([])),
    };
    let (body, problem) = if body.chars().count() <= MAX_BODY_CHARS {
        (body, problem)
    } else {
        let shown: String = body.chars().take(MAX_BODY_CHARS).collect();
        (
            format!("{shown}\n\n…\n\nThe rest of this request is shown only in Terminal."),
            Some(problem.unwrap_or_else(|| {
                "This request is too long to review here. Answer it in Terminal.".to_owned()
            })),
        )
    };
    let supported = problem.is_none();
    let mut card = json!({"question_id":id,"tool_call_id":call,"agent_id":"main",
        "run_id":record["run_id"],"conversation_id":record["conversation_id"],
        "created_at":event["time"].as_f64().unwrap_or(0.0) / 1000.0,"source":kimi_approval::SOURCE,
        "approval":true,"approval_choices":true,"can_answer":false,"answer_transport":"",
        "answer_unavailable_reason":problem.unwrap_or_else(|| "Open Terminal to answer this approval.".into()),
        "questions":[{"id":"goal","question":kimi_approval::title("CreateGoal"),"header":"CreateGoal","body":body,
            "multi_select":false,"allow_other":false,"options":options}]});
    card["question_hash"] = json!(format!(
        "{:x}",
        Sha256::digest(
            json!([
                kimi_approval::SOURCE,
                id,
                call,
                event["agentId"],
                request,
                record["run_id"],
                record["conversation_id"],
                record["pid"],
                record["process_start"],
                record["pane"],
                supported,
                card["questions"]
            ])
            .to_string()
        )
    ));
    Some(card)
}

fn columns(text: &str) -> Option<usize> {
    text.chars().try_fold(0usize, |total, ch| {
        ch.width().and_then(|width| total.checked_add(width))
    })
}

fn truncate(text: &str, width: usize) -> Option<String> {
    if columns(text)? <= width {
        return Some(text.to_owned());
    }
    if width <= 3 {
        return Some(".".repeat(width));
    }
    let target = width - 3;
    let mut used = 0;
    let mut result = String::new();
    for ch in text.chars() {
        let next = ch.width()?;
        if used + next > target {
            break;
        }
        result.push(ch);
        used += next;
    }
    result.push_str("...");
    Some(result)
}

fn rule(row: &str) -> Option<(usize, usize)> {
    let rest = row.trim_start_matches(' ');
    let margin = row.len() - rest.len();
    let width = rest.chars().count();
    (margin <= 8 && width >= 20 && rest.chars().all(|ch| ch == '─')).then_some((margin, width))
}

fn inside(row: &str, margin: usize) -> Option<&str> {
    if row.is_empty() {
        return Some("");
    }
    row.strip_prefix(&" ".repeat(margin))
}

fn choice_row(row: &str, margin: usize) -> Option<(usize, &str, bool)> {
    let row = inside(row, margin)?;
    let (row, selected) = match row.strip_prefix("  ▶ ") {
        Some(row) => (row, true),
        None => (row.strip_prefix("    ")?, false),
    };
    let (number, label) = row.split_once(". ")?;
    if number.is_empty() || !number.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((number.parse().ok()?, label, selected))
}

fn hint(choices: usize) -> String {
    let numbers = (1..=choices)
        .map(|number| number.to_string())
        .collect::<Vec<_>>()
        .join("/");
    format!("  ↑/↓ select · {numbers} choose · ↵ confirm")
}

fn head(goal: &Goal<'_>, width: usize) -> Result<Vec<String>> {
    if width < 20 {
        return Err(NARROW.into());
    }
    let mut rows = vec![
        "─".repeat(width),
        format!("  ▶ {}", kimi_approval::title("CreateGoal")),
        String::new(),
    ];
    for line in goal.body().split('\n') {
        rows.push(format!("  {line}"));
    }
    rows.push(String::new());
    rows.into_iter()
        .map(|row| truncate(&row, width).ok_or_else(|| INVALID.into()))
        .collect()
}

fn chooser(rows: &[&str], margin: usize, width: usize, goal: &Goal<'_>) -> Result<()> {
    let choices = goal.choices();
    let mut index = 0;
    let mut selected = None;
    for (position, choice) in choices.iter().enumerate() {
        let (number, label, marked) =
            choice_row(rows.get(index).ok_or(NARROW)?, margin).ok_or(NARROW)?;
        if number != position + 1 || label != choice.label {
            return Err(
                "Kimi's goal choices differ from this request; answer it in Terminal".into(),
            );
        }
        if marked && selected.replace(position).is_some() {
            return Err(
                "Kimi's goal approval has an ambiguous selection; answer it in Terminal".into(),
            );
        }
        let mut end = index + 1;
        while let Some(row) = rows.get(end) {
            if inside(row, margin).is_some_and(str::is_empty) || choice_row(row, margin).is_some() {
                break;
            }
            end += 1;
        }
        let description: Vec<String> = rows[index + 1..end]
            .iter()
            .map(|row| {
                inside(row, margin)
                    .and_then(|row| row.strip_prefix("       "))
                    .map(str::to_owned)
                    .ok_or_else(|| NARROW.to_owned())
            })
            .collect::<Result<_>>()?;
        if description.is_empty() || !question_terminal::wrapped(&description, choice.description) {
            return Err("Kimi's goal choice descriptions differ; answer it in Terminal".into());
        }
        index = end;
    }
    if selected.is_none()
        || !inside(rows.get(index).ok_or(NARROW)?, margin).is_some_and(str::is_empty)
        || inside(rows.get(index + 1).ok_or(NARROW)?, margin)
            != truncate(&hint(choices.len()), width).as_deref()
        || rule(rows.get(index + 2).ok_or(NARROW)?) != Some((margin, width))
    {
        return Err(NARROW.into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Inline,
    Docked(usize),
}

fn docked_head(goal: &Goal<'_>, width: usize) -> Result<Vec<String>> {
    let mut rows = head(goal, width)?;
    // DockSection's body reserves one column for its scrollbar, including
    // when the complete goal fits and the scrollbar itself is blank.
    for (index, line) in goal.body().split('\n').enumerate() {
        rows[index + 3] = truncate(&format!("  {line}"), width - 1).ok_or(INVALID)?;
    }
    Ok(rows)
}

fn docked_chooser(rows: &[&str], margin: usize, width: usize, goal: &Goal<'_>) -> Result<usize> {
    let choices = goal.choices();
    let mut selected = None;
    for (position, choice) in choices.iter().enumerate() {
        let (number, label, marked) =
            choice_row(rows.get(position).ok_or(NARROW)?, margin).ok_or(NARROW)?;
        if number != position + 1 || label != choice.label {
            return Err(
                "Kimi's goal choices differ from this request; answer it in Terminal".into(),
            );
        }
        if marked && selected.replace(position).is_some() {
            return Err(
                "Kimi's goal approval has an ambiguous selection; answer it in Terminal".into(),
            );
        }
    }
    let selected = selected.ok_or(NARROW)?;
    let count = choices.len();
    if !inside(rows.get(count).ok_or(NARROW)?, margin).is_some_and(str::is_empty) {
        return Err(NARROW.into());
    }
    // Kimi 2.4.1 uses a fixed area of at most three rows, padded below a
    // shorter description. Reject clipped text, hover previews of another
    // choice and changed wording; navigation will verify every hidden choice.
    for height in 1..=3 {
        let start = count + 1;
        let end = start + height;
        if !inside(rows.get(end).copied().unwrap_or("?"), margin).is_some_and(str::is_empty)
            || inside(rows.get(end + 1).copied().unwrap_or("?"), margin)
                != truncate(&hint(count), width).as_deref()
            || rows.get(end + 2).and_then(|row| rule(row)) != Some((margin, width))
        {
            continue;
        }
        let description = rows[start..end]
            .iter()
            .map(|row| {
                inside(row, margin)
                    .and_then(|row| {
                        if row.is_empty() {
                            Some("")
                        } else {
                            row.strip_prefix("  ")
                        }
                    })
                    .ok_or_else(|| NARROW.to_owned())
            })
            .collect::<Result<Vec<_>>>()?;
        let first_blank = description
            .iter()
            .position(|row| row.is_empty())
            .unwrap_or(height);
        if description[first_blank..].iter().all(|row| row.is_empty())
            && question_terminal::wrapped(
                &description[..first_blank],
                choices[selected].description,
            )
        {
            return Ok(selected);
        }
        return Err("Kimi's goal choice description differs; answer it in Terminal".into());
    }
    Err(NARROW.into())
}

fn bottom(rows: &[&str]) -> Option<(usize, usize, usize)> {
    (6..rows.len()).rev().find_map(|end| {
        let (margin, width) = rule(rows[end])?;
        let controls = inside(rows[end - 1], margin)?;
        if !(controls.starts_with("  ↑/↓ select · ") && controls.ends_with(" choose · ↵ confirm"))
            || !inside(rows[end - 2], margin)?.is_empty()
        {
            return None;
        }
        let before = (0..end - 2)
            .rev()
            .find(|index| rule(rows[*index]) == Some((margin, width)))
            .map_or(0, |index| index + 1);
        let first = (before..end - 2).find(|index| choice_row(rows[*index], margin).is_some())?;
        let labels: Vec<&str> = rows[first..end - 2]
            .iter()
            .filter_map(|row| choice_row(row, margin))
            .filter_map(|(_, label, _)| {
                CHOICES
                    .iter()
                    .any(|choice| choice.label == label)
                    .then_some(label)
            })
            .collect();
        (matches!(labels.len(), 3 | 4) && labels.last() == Some(&CANCEL.label))
            .then_some((first, margin, width))
    })
}

fn top(rows: &[&str], first: usize, margin: usize, width: usize) -> Option<usize> {
    let start = rows[..first.saturating_sub(1)]
        .iter()
        .rposition(|row| rule(row) == Some((margin, width)))?;
    (inside(rows.get(start + 1)?, margin)? == format!("  ▶ {}", kimi_approval::title("CreateGoal"))
        && inside(rows.get(start + 2)?, margin)?.is_empty())
    .then_some(start)
}

fn complete(screen: &str) -> bool {
    let Ok(plain) = question_terminal::plain(screen) else {
        return false;
    };
    let rows: Vec<&str> = plain.lines().map(str::trim_end).collect();
    bottom(&rows).is_some_and(|(first, margin, width)| top(&rows, first, margin, width).is_some())
}

fn clipped(screen: &str) -> bool {
    let Ok(plain) = question_terminal::plain(screen) else {
        return false;
    };
    let rows: Vec<&str> = plain.lines().map(str::trim_end).collect();
    bottom(&rows).is_some_and(|(first, margin, width)| top(&rows, first, margin, width).is_none())
}

fn read_layout(screen: &str, request: &Value) -> Result<Layout> {
    let goal = goal(request)?;
    if goal.body().chars().count() > MAX_BODY_CHARS {
        return Err("This request is too long to review here. Answer it in Terminal.".into());
    }
    let plain = question_terminal::plain(screen)?;
    let rows: Vec<&str> = plain.lines().map(str::trim_end).collect();
    let mut found = Vec::new();
    for (start, row) in rows.iter().enumerate() {
        let Some((margin, width)) = rule(row) else {
            continue;
        };
        for docked in [false, true] {
            let Ok(head) = (if docked {
                docked_head(&goal, width)
            } else {
                head(&goal, width)
            }) else {
                continue;
            };
            if !head.iter().enumerate().all(|(index, expected)| {
                rows.get(start + index).and_then(|row| inside(row, margin))
                    == Some(expected.as_str())
            }) {
                continue;
            }
            let rest = &rows[start + head.len()..];
            if docked {
                if let Ok(selected) = docked_chooser(rest, margin, width, &goal) {
                    found.push(Layout::Docked(selected));
                }
            } else if chooser(rest, margin, width, &goal).is_ok() {
                found.push(Layout::Inline);
            }
        }
    }
    match found.as_slice() {
        [layout] => Ok(*layout),
        [] if clipped(screen) => Err(NARROW.into()),
        [] if complete(screen) => Err(
            "The visible Kimi goal approval differs from this request; answer it in Terminal"
                .into(),
        ),
        [] => Err("Kimi's goal approval is not visible; open Terminal".into()),
        _ => Err("Kimi shows this goal approval more than once; answer it in Terminal".into()),
    }
}

fn read(screen: &str, request: &Value) -> Result<()> {
    read_layout(screen, request).map(|_| ())
}

fn chosen(card: &Value, answers: &Value) -> Result<(usize, String)> {
    let answer = answers
        .as_array()
        .filter(|items| items.len() == 1)
        .map(|items| &items[0])
        .ok_or("answer the goal approval")?;
    if string(answer, "question_id") != "goal" || !string(answer, "text").is_empty() {
        return Err("choose one goal action".into());
    }
    let ids = answer["selected_option_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let [id] = ids.as_slice() else {
        return Err("choose one goal action".into());
    };
    let id = id.as_str().ok_or("unknown goal action")?;
    let index = card["questions"][0]["options"]
        .as_array()
        .and_then(|options| options.iter().position(|option| option["id"] == id))
        .ok_or("unknown goal action")?;
    Ok((index, id.to_owned()))
}

pub(super) fn response_matches(card: &Value, answers: &Value, event: &Value) -> bool {
    let Ok((_, id)) = chosen(card, answers) else {
        return false;
    };
    let response = &event["response"];
    string(event, "type") == "interaction.resolved"
        && string(response, "decision")
            == if id == "cancel" {
                "cancelled"
            } else {
                "approved"
            }
        && string(response, "selectedLabel") == id
        && response
            .as_object()
            .is_some_and(|response| response.len() == 2)
}

pub(super) fn journal_response(card: &Value, event: &Value) -> Value {
    let selected = string(&event["response"], "selectedLabel");
    let label = card["questions"][0]["options"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|option| string(option, "id") == selected)
        .map(|option| string(option, "label"))
        .unwrap_or(selected);
    json!({"time":event["time"],"response":{"answers":{string(&card["questions"][0], "question"):label}}})
}

pub(super) fn fit(record: &Value, card: &Value) {
    if !is(card) {
        return;
    }
    let Ok(current) = Native::new(record, card).identity() else {
        return;
    };
    let Ok(event) = kimi_approval::bound_goal(&current, card) else {
        return;
    };
    let Ok(goal) = goal(&event["request"]) else {
        return;
    };
    if goal.body().chars().count() > MAX_BODY_CHARS {
        return;
    }
    let Ok(screen) = Native::capture(&current) else {
        return;
    };
    if clipped(&screen) {
        question_terminal::fit_unviewed(&current, screen, Native::capture, clipped, complete);
    }
}

pub(super) fn available(record: &Value, card: &Value) -> Result<()> {
    if string(record, "agent") != "kimi" || !is(card) {
        return Err("Not a Kimi goal approval".into());
    }
    let current = Native::new(record, card).identity()?;
    let event = kimi_approval::bound_goal(&current, card)?;
    read(&Native::capture(&current)?, &event["request"])
}

pub(super) fn answer(record: &Value, card: &Value, answers: &Value) -> Result<()> {
    available(record, card)?;
    let guard = Native::new(record, card);
    let request = kimi_approval::bound_goal(&guard.identity()?, card)?["request"].clone();
    let mut check = || kimi_approval::bound_goal(&guard.identity()?, card).map(|_| ());
    let mut native = Native::new(record, card);
    drive(&mut native, &mut check, &request, card, answers).map_err(|error| {
        if native.touched() {
            format!("delivery uncertain: {error}; inspect Terminal and do not resend automatically")
        } else {
            error
        }
    })
}

fn drive<T: Transport>(
    io: &mut T,
    check: &mut dyn FnMut() -> Result<()>,
    request: &Value,
    card: &Value,
    answers: &Value,
) -> Result<()> {
    let (index, _) = chosen(card, answers)?;
    if let Layout::Docked(mut selected) = read_layout(&io.screen()?, request)? {
        let count = goal(request)?.choices().len();
        // Inspection never navigates. Once the user explicitly answers, check
        // every description the dock hides before a number key can submit.
        for _ in 1..count {
            selected = next_docked(io, check, request, selected, count)?;
        }
        for _ in 0..count {
            if selected == index {
                break;
            }
            selected = next_docked(io, check, request, selected, count)?;
        }
        if read_layout(&io.screen()?, request)? != Layout::Docked(index) {
            return Err("Kimi's goal selection changed before submission".into());
        }
    }
    check()?;
    io.key(&(index + 1).to_string())?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(response) = io.resolved()? {
            let event = json!({"type":"interaction.resolved","response":response});
            if !response_matches(card, answers, &event) {
                return Err("Kimi recorded a different goal decision".into());
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("Kimi has not confirmed the goal decision".into());
        }
        io.pause();
    }
}

fn next_docked<T: Transport>(
    io: &mut T,
    check: &mut dyn FnMut() -> Result<()>,
    request: &Value,
    selected: usize,
    count: usize,
) -> Result<usize> {
    check()?;
    io.key("Down")?;
    let next = (selected + 1) % count;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        io.pause();
        match read_layout(&io.screen()?, request)? {
            Layout::Docked(actual) if actual == next => return Ok(next),
            Layout::Docked(actual) if actual == selected => {}
            _ => return Err("Kimi's goal panel changed during navigation".into()),
        }
        if Instant::now() >= deadline {
            return Err("Kimi's goal selection did not move".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn record(home: &Path) -> Value {
        json!({"agent":"kimi","agent_home":home,"run_id":"run","conversation_id":"session_test","pid":42,
            "process_start":"start","pane":"%3","active_tools":{"tool_goal":{"name":"CreateGoal","started":1000.0}}})
    }

    fn request(mode: &str) -> Value {
        json!({"type":"interaction.request","agentId":"main","id":"approval_goal","kind":"approval","toolCallId":"tool_goal",
            "request":{"id":"approval_goal","sessionId":"session_test","agentId":"main","turnId":1,"toolCallId":"tool_goal",
                "toolName":"CreateGoal","action":"Creating a goal","display":{"kind":"goal_start",
                "objective":"Завершить проверку RC12 без production","completionCriterion":"Все 13 trophy/156 состояний проверены, включая g2★0","mode":mode}},
            "time":1_000_500})
    }

    fn only(record: &Value, event: &Value) -> Value {
        let cards = recover(record, std::slice::from_ref(event));
        let [card] = cards.as_slice() else {
            panic!("{cards:?}")
        };
        card.clone()
    }

    fn screen(request: &Value, selected: usize) -> String {
        let goal = goal(&request["request"]).unwrap();
        let width = 200;
        let rule = "─".repeat(width);
        let mut rows = vec![
            "transcript".to_owned(),
            rule.clone(),
            format!("  ▶ {}", kimi_approval::title("CreateGoal")),
            String::new(),
        ];
        rows.extend(goal.body().split('\n').map(|line| format!("  {line}")));
        rows.push(String::new());
        for (index, choice) in goal.choices().iter().enumerate() {
            rows.push(format!(
                "{}{}. {}",
                if index == selected { "  ▶ " } else { "    " },
                index + 1,
                choice.label
            ));
            rows.push(format!("       {}", choice.description));
        }
        rows.extend([
            String::new(),
            hint(goal.choices().len()),
            rule,
            " Ask When Needed goal  K3".to_owned(),
        ]);
        rows.into_iter()
            .map(|row| {
                if row.is_empty() {
                    row
                } else {
                    format!(" {row}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn docked_screen(request: &Value, selected: usize, width: usize) -> String {
        let goal = goal(&request["request"]).unwrap();
        let choices = goal.choices();
        let wrap = |text: &str| {
            let mut rows = vec![String::new()];
            for word in text.split_whitespace() {
                let last = rows.last_mut().unwrap();
                if !last.is_empty() && last.len() + 1 + word.len() > width - 2 {
                    rows.push(word.to_owned());
                } else {
                    if !last.is_empty() {
                        last.push(' ');
                    }
                    last.push_str(word);
                }
            }
            rows
        };
        let mut rows = docked_head(&goal, width).unwrap();
        for (index, choice) in choices.iter().enumerate() {
            rows.push(format!(
                "{}{}. {}",
                if index == selected { "  ▶ " } else { "    " },
                index + 1,
                choice.label
            ));
        }
        rows.push(String::new());
        let height = choices
            .iter()
            .map(|choice| wrap(choice.description).len())
            .max()
            .unwrap()
            .min(3);
        let description = wrap(choices[selected].description);
        for index in 0..height {
            rows.push(
                description
                    .get(index)
                    .map(|row| format!("  {row}"))
                    .unwrap_or_default(),
            );
        }
        rows.extend([String::new(), hint(choices.len()), "─".repeat(width)]);
        rows.into_iter()
            .map(|row| {
                if row.is_empty() {
                    row
                } else {
                    format!(" {row}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn fullscreen_panel_binds_the_selected_description_and_body_width() {
        const CAPTURE: &str =
            include_str!("../../tests/fixtures/kimi-goal-2.4.1-fullscreen-manual-78x24.txt");
        let mut event = request("manual");
        event["request"]["display"]["objective"] =
            json!("Завершить проверку отдельного intrinsic слоя и новой публикации без production");
        event["request"]["display"]["completionCriterion"] =
            json!("Все 13 trophy/156 состояний проверены, включая g2★0, без двойного учёта");
        assert_eq!(
            read_layout(CAPTURE, &event["request"]),
            Ok(Layout::Docked(0))
        );
        assert!(complete(CAPTURE) && !clipped(CAPTURE));
        for mode in ["manual", "yolo"] {
            let event = request(mode);
            for selected in 0..goal(&event["request"]).unwrap().choices().len() {
                let screen = docked_screen(&event, selected, 78);
                assert_eq!(
                    read_layout(&screen, &event["request"]),
                    Ok(Layout::Docked(selected)),
                    "{screen}"
                );
                for wrong in [
                    screen.replace("RC12", "other"),
                    screen.replace("Do not start", "Do not begin"),
                    screen.replace("▶ ", "  "),
                    screen.replace("↑/↓ select", "↑/↓ choose"),
                    format!("{screen}\n{screen}"),
                ] {
                    assert!(read(&wrong, &event["request"]).is_err(), "{wrong}");
                }
            }
            let shown = docked_screen(&event, 0, 78);
            assert!(read(
                &shown.replace("questions are skipped", "questions are ignored"),
                &event["request"]
            )
            .is_err());
            let clipped_screen = shown.lines().skip(1).collect::<Vec<_>>().join("\n");
            assert!(clipped(&clipped_screen) && !complete(&clipped_screen));
            assert!(read(&docked_screen(&event, 0, 40), &event["request"]).is_err());
        }
    }

    fn answer(id: &str) -> Value {
        json!([{"question_id":"goal","selected_option_ids":[id],"text":""}])
    }

    fn response(id: &str) -> Value {
        json!({"type":"interaction.resolved","response":{
            "decision":if id == "cancel" {"cancelled"} else {"approved"},"selectedLabel":id}})
    }

    #[test]
    fn recovery_keeps_only_the_live_goal_request() {
        let home = tempfile::tempdir().unwrap();
        let record = record(home.path());
        let event = request("yolo");
        assert_eq!(only(&record, &event)["question_id"], "approval_goal");
        for closure in [
            json!({"type":"interaction.resolved","id":"approval_goal"}),
            json!({"type":"interaction.cancelled","id":"approval_goal"}),
            json!({"type":"interaction.dismissed","id":"approval_goal"}),
        ] {
            assert!(recover(&record, &[event.clone(), closure]).is_empty());
        }
        let mut inactive = record.clone();
        inactive["active_tools"] = json!({});
        assert!(recover(&inactive, std::slice::from_ref(&event)).is_empty());
        let mut child = event.clone();
        child["agentId"] = json!("child");
        assert!(recover(&record, &[child]).is_empty());
        let mut optional = event.clone();
        optional["request"]["display"]
            .as_object_mut()
            .unwrap()
            .remove("completionCriterion");
        assert!(
            string(&only(&record, &optional)["questions"][0], "body").starts_with("Start goal: ")
        );
        assert!(!string(&only(&record, &optional)["questions"][0], "body").contains("Done when:"));
        for (path, value) in [
            ("/request/display/mode", json!("auto")),
            ("/request/display/objective", json!(" \n")),
            ("/request/display/objective", json!("bad\u{200d}joiner")),
            ("/request/display/extra", json!(true)),
        ] {
            let mut invalid = event.clone();
            match invalid.pointer_mut(path) {
                Some(slot) => *slot = value,
                None => invalid["request"]["display"]["extra"] = value,
            }
            let card = only(&record, &invalid);
            assert!(card["questions"][0]["options"]
                .as_array()
                .unwrap()
                .is_empty());
            assert_eq!(card["can_answer"], false);
            assert_eq!(card["answer_unavailable_reason"], INVALID);
            assert!(read(&screen(&event, 0), &invalid["request"]).is_err());
        }
    }

    #[test]
    fn an_oversized_goal_stays_visible_but_terminal_only() {
        let home = tempfile::tempdir().unwrap();
        let record = record(home.path());
        let mut event = request("yolo");
        event["request"]["display"]["objective"] = json!("x".repeat(MAX_BODY_CHARS));
        let card = only(&record, &event);
        assert!(string(&card["questions"][0], "body")
            .ends_with("The rest of this request is shown only in Terminal."));
        assert_eq!(card["can_answer"], false);
        assert!(string(&card, "answer_unavailable_reason").contains("too long"));
        assert!(read(&screen(&event, 0), &event["request"])
            .unwrap_err()
            .contains("too long"));

        event["request"]["display"]["extra"] = json!(true);
        let invalid = only(&record, &event);
        assert!(string(&invalid["questions"][0], "body")
            .ends_with("The rest of this request is shown only in Terminal."));
        assert!(string(&invalid["questions"][0], "body").chars().count() < MAX_BODY_CHARS + 100);
        assert_eq!(invalid["answer_unavailable_reason"], INVALID);
    }

    #[test]
    fn goal_panel_must_match_the_request_and_every_choice() {
        for mode in ["yolo", "manual"] {
            let event = request(mode);
            let shown = screen(&event, 0);
            assert!(complete(&shown) && !clipped(&shown));
            assert_eq!(read(&shown, &event["request"]), Ok(()));
            assert_eq!(read(&screen(&event, 1), &event["request"]), Ok(()));
            for wrong in [
                shown.replace("Завершить проверку", "Завершить другую проверку"),
                shown.replace("13 trophy/156", "14 trophy/156"),
                shown.replace("Do not start", "Do not begin"),
                shown.replace("questions are skipped", "questions are ignored"),
                shown.replacen("▶ 1.", "  1.", 1),
                shown.replacen("    2.", "  ▶ 2.", 1),
                shown.replace("↑/↓ select", "↑/↓ choose"),
                format!("{shown}\n{shown}"),
            ] {
                assert!(read(&wrong, &event["request"]).is_err(), "{wrong}");
            }
            let clipped_screen = shown.lines().skip(2).collect::<Vec<_>>().join("\n");
            assert!(clipped(&clipped_screen) && !complete(&clipped_screen));
            assert_eq!(
                read(&clipped_screen, &event["request"]).unwrap_err(),
                NARROW
            );
        }
    }

    #[test]
    fn kimi_2_4_1_goal_panel_matches_exactly() {
        // Kimi Code 2.4.1's own 78x24 approval panel, captured by the native CI fixture.
        const SCREEN: &str = include_str!("../../tests/fixtures/kimi-goal-2.4.1-manual-78x24.txt");
        let mut event = request("manual");
        event["request"]["display"]["objective"] =
            json!("Завершить проверку отдельного intrinsic слоя и новой публикации без production");
        event["request"]["display"]["completionCriterion"] =
            json!("Все 13 trophy/156 состояний проверены, включая g2★0, без двойного учёта");
        assert!(complete(SCREEN) && !clipped(SCREEN));
        assert_eq!(read(SCREEN, &event["request"]), Ok(()));
        for older in [
            SCREEN.replace(
                "automatically and questions",
                "automatically, and questions",
            ),
            SCREEN.replace(
                "  Tools are approved",
                "  Tools and plan changes are approved",
            ),
            SCREEN.replace("goal command.", "goal command"),
        ] {
            assert!(read(&older, &event["request"]).is_err(), "{older}");
        }
    }

    #[test]
    fn truncation_and_unicode_follow_visible_columns() {
        assert_eq!(truncate("abcdef", 5).as_deref(), Some("ab..."));
        assert_eq!(truncate("Привет", 5).as_deref(), Some("Пр..."));
        assert_eq!(truncate("g2★0xyz", 5).as_deref(), Some("g2..."));
        assert_eq!(truncate("界ab", 2).as_deref(), Some(".."));
        assert_eq!(truncate("界ab", 4).as_deref(), Some("界ab"));
        assert_eq!(truncate("界abc", 4).as_deref(), Some("..."));
        assert_eq!(truncate("short", 20).as_deref(), Some("short"));
        let mut event = request("yolo");
        event["request"]["display"]["objective"] =
            json!("Завершить проверку отдельного intrinsic слоя и новой публикации без production");
        let goal = goal(&event["request"]).unwrap();
        let width = 60;
        let head = head(&goal, width).unwrap();
        assert!(head[3].ends_with("..."));
        assert!(head[3].contains("Завершить проверку"));
        assert_eq!(columns(&head[3]), Some(width));
        let full = screen(&event, 0);
        let mut rows: Vec<String> = full.lines().map(str::to_owned).collect();
        rows[1] = format!(" {}", "─".repeat(width));
        rows[2] = format!(" {}", head[1]);
        rows[3] = String::new();
        rows[4] = format!(" {}", head[3]);
        rows[5] = format!(" {}", head[4]);
        rows[6] = String::new();
        let closing = rows.len() - 2;
        rows[closing] = format!(" {}", "─".repeat(width));
        let narrow = rows.join("\n");
        assert_eq!(read(&narrow, &event["request"]), Ok(()));
    }

    #[test]
    fn acknowledgement_is_the_exact_native_mode_decision() {
        let home = tempfile::tempdir().unwrap();
        let record = record(home.path());
        for mode in ["yolo", "manual"] {
            let card = only(&record, &request(mode));
            for option in card["questions"][0]["options"].as_array().unwrap() {
                let id = string(option, "id");
                let answers = answer(id);
                let resolved = response(id);
                assert!(response_matches(&card, &answers, &resolved), "{id}");
                assert_eq!(
                    journal_response(&card, &resolved)["response"]["answers"]
                        ["Approve CreateGoal?"],
                    option["label"]
                );
                assert!(!response_matches(&card, &answers, &response("other")));
                let mut extra = resolved.clone();
                extra["response"]["scope"] = json!("session");
                assert!(!response_matches(&card, &answers, &extra));
                let mut decision = resolved.clone();
                decision["response"]["decision"] = json!("approved");
                assert_eq!(response_matches(&card, &answers, &decision), id != "cancel");
            }
        }
        let card = only(&record, &request("yolo"));
        assert!(!response_matches(&card, &json!([]), &response("auto")));
        assert!(!response_matches(
            &card,
            &answer("unknown"),
            &response("unknown")
        ));
        let mut wrong_question = answer("auto");
        wrong_question[0]["question_id"] = json!("approval");
        assert!(!response_matches(&card, &wrong_question, &response("auto")));
        let mut text = answer("auto");
        text[0]["text"] = json!("start");
        assert!(!response_matches(&card, &text, &response("auto")));
    }

    struct Fixture {
        screens: Vec<String>,
        keys: Vec<String>,
        pastes: Vec<String>,
        response: Option<Value>,
    }

    impl Transport for Fixture {
        fn screen(&mut self) -> Result<String> {
            Ok(self.screens[0].clone())
        }
        fn key(&mut self, key: &str) -> Result<()> {
            self.keys.push(key.into());
            if self.screens.len() > 1 {
                self.screens.remove(0);
            }
            Ok(())
        }
        fn paste(&mut self, text: &str) -> Result<()> {
            self.pastes.push(text.into());
            Ok(())
        }
        fn resolved(&mut self) -> Result<Option<Value>> {
            Ok(self.response.clone().filter(|_| self.screens.len() == 1))
        }
        fn pause(&mut self) {}
    }

    fn docked_delivery(event: &Value, initial: usize, target: usize) -> Fixture {
        let count = goal(&event["request"]).unwrap().choices().len();
        let mut selections = vec![initial];
        for _ in 1..count {
            selections.push((selections.last().unwrap() + 1) % count);
        }
        for _ in 0..count {
            if selections.last() == Some(&target) {
                break;
            }
            selections.push((selections.last().unwrap() + 1) % count);
        }
        let mut screens = selections
            .into_iter()
            .map(|selected| docked_screen(event, selected, 78))
            .collect::<Vec<_>>();
        screens.push(String::new());
        Fixture {
            screens,
            keys: vec![],
            pastes: vec![],
            response: Some(
                response(goal(&event["request"]).unwrap().choices()[target].id)["response"].clone(),
            ),
        }
    }

    #[test]
    fn a_skipped_navigation_frame_never_submits() {
        let home = tempfile::tempdir().unwrap();
        let record = record(home.path());
        for mode in ["manual", "yolo"] {
            let event = request(mode);
            let card = only(&record, &event);
            let mut io = docked_delivery(&event, 0, 0);
            io.screens[1] = docked_screen(&event, 2, 78);
            assert!(drive(
                &mut io,
                &mut || Ok(()),
                &event["request"],
                &card,
                &answer("auto")
            )
            .is_err());
            assert_eq!(
                io.keys,
                ["Down"],
                "a skipped choice must stop before the next key"
            );
        }
    }

    #[test]
    fn fullscreen_delivery_checks_every_description_before_the_submission_key() {
        let home = tempfile::tempdir().unwrap();
        let record = record(home.path());
        for mode in ["manual", "yolo"] {
            let event = request(mode);
            let card = only(&record, &event);
            let choices = goal(&event["request"]).unwrap().choices();
            for initial in 0..choices.len() {
                for (target, choice) in choices.iter().enumerate() {
                    let mut io = docked_delivery(&event, initial, target);
                    let navigation = io.screens.len() - 2;
                    let mut checks = 0;
                    drive(
                        &mut io,
                        &mut || {
                            checks += 1;
                            Ok(())
                        },
                        &event["request"],
                        &card,
                        &answer(choice.id),
                    )
                    .unwrap();
                    assert_eq!(&io.keys[..navigation], vec!["Down"; navigation]);
                    assert_eq!(io.keys[navigation], (target + 1).to_string());
                    assert_eq!(checks, io.keys.len());
                    assert!(io.pastes.is_empty());
                }
            }
            let mut io = docked_delivery(&event, 0, 0);
            io.screens[1] = io.screens[1].replace(
                "approved automatically.",
                "approved automatically and plans too.",
            );
            assert!(drive(
                &mut io,
                &mut || Ok(()),
                &event["request"],
                &card,
                &answer("auto")
            )
            .is_err());
            assert_eq!(
                io.keys,
                ["Down"],
                "a changed hidden description must never submit"
            );

            let mut io = docked_delivery(&event, 0, 0);
            let mut checks = 0;
            assert!(drive(
                &mut io,
                &mut || {
                    checks += 1;
                    if checks == 2 {
                        Err("request changed".into())
                    } else {
                        Ok(())
                    }
                },
                &event["request"],
                &card,
                &answer("auto")
            )
            .is_err());
            assert_eq!(
                io.keys,
                ["Down"],
                "a stale request stops before the next key"
            );

            let mut io = docked_delivery(&event, 0, 0);
            io.screens[1] = screen(&event, 1);
            assert!(drive(
                &mut io,
                &mut || Ok(()),
                &event["request"],
                &card,
                &answer("auto")
            )
            .is_err());
            assert_eq!(io.keys, ["Down"], "a layout transition is not replayed");
        }
    }

    #[test]
    fn delivery_uses_one_key_after_the_exact_guard() {
        let home = tempfile::tempdir().unwrap();
        let record = record(home.path());
        for mode in ["yolo", "manual"] {
            let event = request(mode);
            let card = only(&record, &event);
            for (index, option) in card["questions"][0]["options"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
            {
                let id = string(option, "id");
                let mut io = Fixture {
                    screens: vec![screen(&event, 0), String::new()],
                    keys: vec![],
                    pastes: vec![],
                    response: Some(response(id)["response"].clone()),
                };
                let mut checks = 0;
                drive(
                    &mut io,
                    &mut || {
                        checks += 1;
                        Ok(())
                    },
                    &event["request"],
                    &card,
                    &answer(id),
                )
                .unwrap();
                assert_eq!(io.keys, [(index + 1).to_string()]);
                assert!(io.pastes.is_empty());
                assert_eq!(checks, 1);
            }
        }
        let event = request("yolo");
        let card = only(&record, &event);
        let mut io = Fixture {
            screens: vec![screen(&event, 0)],
            keys: vec![],
            pastes: vec![],
            response: None,
        };
        assert!(drive(
            &mut io,
            &mut || Err("changed".into()),
            &event["request"],
            &card,
            &answer("auto")
        )
        .is_err());
        assert!(io.keys.is_empty());
        let mut io = Fixture {
            screens: vec![screen(&event, 0).replace("RC12", "other")],
            keys: vec![],
            pastes: vec![],
            response: None,
        };
        assert!(drive(
            &mut io,
            &mut || Ok(()),
            &event["request"],
            &card,
            &answer("auto")
        )
        .is_err());
        assert!(io.keys.is_empty());
        let mut io = Fixture {
            screens: vec![screen(&event, 0), String::new()],
            keys: vec![],
            pastes: vec![],
            response: Some(json!({"decision":"approved","selectedLabel":"yolo"})),
        };
        assert!(drive(
            &mut io,
            &mut || Ok(()),
            &event["request"],
            &card,
            &answer("auto")
        )
        .is_err());
        assert_eq!(io.keys, ["1"]);
    }
}
