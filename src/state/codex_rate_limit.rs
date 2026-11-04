//! Runtime Codex lower-credit model chooser. Detection never chooses a model.
//! Only the reviewed complete panel is answerable; changed/clipped panels keep
//! attention with an explicit Terminal handoff, including after a native Stop.
use super::*;
use sha2::{Digest, Sha256};

const TITLE: &str = "Approaching rate limits";
const HINT: &str = "enter select · esc back";
const SUBTITLE: &str = "Switch to gpt-6-luna for lower credit usage?";
const IDS: [&str; 3] = ["switch_model", "keep_model", "keep_model_never"];
const LABELS: [&str; 3] = [
    "Switch to gpt-6-luna",
    "Keep current model",
    "Keep current model (never show again)",
];
const DESCRIPTIONS: [&str; 3] = [
    "Fast and affordable model for easier tasks.",
    "",
    "Hide future rate limit reminders about switching models",
];
pub(super) const WAIT: &str = "Codex is waiting for your rate-limit model choice. Choose an option in Activity or open Terminal before sending a message.";

struct Panel {
    // Native panel text only, excluding the moving selection marker.
    text: String,
    selected: Option<usize>,
}

fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse(screen: &str) -> Option<Panel> {
    let rows: Vec<_> = screen.lines().map(str::trim_end).collect();
    let end = rows.iter().rposition(|row| !row.is_empty())?;
    let start = (0..=end).rev().find(|&i| rows[i] == format!("  {TITLE}"))?;
    // This bottom-pane picker replaces the composer. A transcript quotation
    // followed by the ordinary composer or further output is not this panel.
    if end - start > 32
        || rows[start..=end].iter().any(|row| {
            row.starts_with("› ")
                && !row
                    .strip_prefix("› ")
                    .unwrap()
                    .starts_with(|c: char| c.is_ascii_digit())
        })
    {
        return None;
    }
    let panel = &rows[start..=end];
    // Incomplete/changed frames remain read-only only when there is native
    // picker evidence as well as the exact heading at its native indentation.
    if !panel.iter().any(|r| {
        r.strip_prefix("› ")
            .and_then(|r| r.split_once(". "))
            .is_some_and(|(number, _)| {
                !number.is_empty()
                    && number.len() <= 2
                    && number.bytes().all(|b| b.is_ascii_digit())
            })
            || r.trim() == HINT
    }) {
        return None;
    }
    let text = panel
        .iter()
        .map(|r| {
            r.strip_prefix("› ")
                .map_or_else(|| (*r).to_owned(), |r| format!("  {r}"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut selected = None;
    let complete = (|| {
        if panel.last()?.trim() != HINT {
            return None;
        }
        let body: Vec<_> = panel[1..panel.len() - 1]
            .iter()
            .copied()
            .filter(|r| !r.is_empty())
            .collect();
        let mut starts = Vec::new();
        for (i, row) in body.iter().enumerate() {
            let (marked, rest) = if let Some(rest) = row.strip_prefix("› ") {
                (true, rest)
            } else {
                (false, row.strip_prefix("  ")?)
            };
            if rest.starts_with(&format!("{}. ", starts.len() + 1)) && starts.len() < 3 {
                if marked && selected.replace(starts.len()).is_some() {
                    return None;
                }
                starts.push(i);
            } else if marked || rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return None;
            }
        }
        if starts.len() != 3 || words(&body[..starts[0]].join(" ")) != SUBTITLE {
            return None;
        }
        for i in 0..3 {
            let stop = starts.get(i + 1).copied().unwrap_or(body.len());
            let rows = &body[starts[i]..stop];
            let first = rows[0].strip_prefix("› ").unwrap_or(rows[0].trim_start());
            let actual = words(
                &std::iter::once(first)
                    .chain(rows[1..].iter().copied())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            let expected = words(&format!("{}. {} {}", i + 1, LABELS[i], DESCRIPTIONS[i]));
            if actual != expected {
                return None;
            }
        }
        selected
    })();
    Some(Panel {
        text,
        selected: complete,
    })
}

fn eligible(record: &Value) -> bool {
    string(record, "agent") == "codex"
        && record["run_identity_version"] == 1
        && record["supervisor"].is_object()
        && !string(record, "run_id").is_empty()
        && string(record, "error").is_empty()
        && !pause_active(record)
        && record.get("session_end").is_none()
}

fn card(record: &Value, panel: &Panel) -> Option<Value> {
    if !eligible(record) {
        return None;
    }
    let hash = format!(
        "{:x}",
        Sha256::digest(
            json!([
                record["run_id"],
                record["pane"],
                record["pid"],
                record["process_start"],
                record["conversation_id"],
                record["expected_id"],
                record["launch_dir"],
                record["agent_home"],
                panel.text
            ])
            .to_string()
        )
    );
    let supported = panel.selected.is_some();
    Some(
        json!({"question_id":format!("codex-rate-limit:{hash}"),"question_hash":hash,
        "tool_call_id":"codex_rate_limit","agent_id":"main","run_id":record["run_id"],
        "conversation_id":record["conversation_id"],"created_at":record["created"].as_f64().unwrap_or(0.),
        "source":"codex_rate_limit","can_answer":supported,"answer_transport":if supported {"codex_tui"} else {""},
        "answer_unavailable_reason":if supported {""} else {"This Codex chooser changed or is incomplete. Open Terminal to answer it."},
        "questions":[{"id":"codex_rate_limit","header":"Rate limits","question":TITLE,
            "body":if supported {SUBTITLE} else {&panel.text},"multi_select":false,"allow_other":false,
            "options":if supported {(0..3).map(|i|json!({"id":IDS[i],"label":LABELS[i],"description":DESCRIPTIONS[i]})).collect::<Vec<_>>()} else {Vec::new()}}]}),
    )
}

fn snapshot(record: &Value, screen: &str) -> Option<(Value, usize)> {
    let panel = parse(screen)?;
    Some((
        card(record, &panel)?["question_hash"].clone(),
        panel.selected?,
    ))
}

pub(super) fn current(record: &Value) -> Option<Value> {
    if !eligible(record) {
        return None;
    }
    let panel = parse(&kimi_tui_choice::capture(record).ok()?)?;
    if !process_alive(record) || !matches(record, live().ok()?.get(string(record, "name"))) {
        return None;
    }
    input::checked_terminal(record, false).ok()?;
    card(record, &panel)
}

pub(super) fn available(record: &Value, question: &Value) -> Result<()> {
    let active = kimi_tui_choice::identity(record)?;
    if question["can_answer"] != true
        || snapshot(&active, &kimi_tui_choice::capture(&active)?).map(|s| s.0)
            != Some(question["question_hash"].clone())
    {
        return Err("Codex's rate-limit chooser changed; open Terminal. Nothing was sent".into());
    }
    Ok(())
}

pub(super) fn answer(record: &Value, question: &Value, answers: &Value) -> Result<()> {
    let chosen = IDS
        .iter()
        .position(|id| Some(*id) == answers[0]["selected_option_ids"][0].as_str())
        .ok_or("Choose a rate-limit model option")?;
    kimi_tui_choice::answer(
        record,
        question,
        chosen,
        IDS.len(),
        snapshot,
        |screen| !screen.trim().is_empty() && !screen.contains(TITLE) && !screen.contains(HINT),
        false,
    )
}

#[cfg(test)]
mod tests;
