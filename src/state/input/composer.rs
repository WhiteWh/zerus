//! The native composer's current text, read from one terminal frame
//! (zerus-wkj9l). Empty follows each agent's verified empty prompt
//! (`composer_empty_for_model`); otherwise the text is recognized per agent or
//! not at all, and an unrecognized layout is never cleared.
use super::*;

/// What one frame shows in the composer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Composer {
    /// The agent's verified empty prompt (a dim placeholder counts as empty).
    Empty,
    /// Recognized text, bounded to `settle::RESIDUE_CHARS` characters.
    Text(String),
    /// A layout this reader does not recognize: never cleared.
    Unknown,
}

/// The agents whose composer can be read and cleared.
pub(crate) fn supported(agent: &str) -> bool {
    matches!(agent, "claude" | "codex" | "kimi")
}

pub(crate) fn read(record: &Value, screen: &str, cursor_x: usize, cursor_y: usize) -> Composer {
    let agent = string(record, "agent");
    if !supported(agent) {
        return Composer::Unknown;
    }
    if composer_empty_for_model(agent, screen, cursor_x, cursor_y, codex_footer_model(record)) {
        return Composer::Empty;
    }
    let text = match agent {
        "claude" => claude_text(screen, cursor_y),
        // `codex_draft` reads plain rows; the empty check above needs the styles.
        "codex" => {
            let plain = screen
                .lines()
                .map(|row| styled_row(row).iter().map(|(ch, _)| ch).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n");
            codex_draft(record, &plain, cursor_y)
        }
        _ => kimi_text(screen, cursor_y),
    };
    match text.map(|text| text.chars().take(settle::RESIDUE_CHARS).collect::<String>()) {
        Some(text) if !text.trim().is_empty() => Composer::Text(text),
        _ => Composer::Unknown,
    }
}

/// Claude Code's composer between its two rules (zerus-wkj9l.3): the cursor
/// row lies below an unindented rule whose next row is the `❯` prompt, and
/// above the next such rule, as `composer_empty_for_model` requires of the
/// empty prompt. A past prompt that history draws as `❯ text`, or a picker or
/// dialog without both rules around the cursor, is never read as composer text.
/// The send path keeps `settle::composer_text`, which also reads simpler layouts.
fn claude_text(screen: &str, cursor_y: usize) -> Option<String> {
    let rows: Vec<String> = screen.lines().map(settle::plain).collect();
    if cursor_y >= rows.len() {
        return None;
    }
    let top = (0..cursor_y)
        .rev()
        .find(|&index| settle::separator(&rows[index]))?;
    let bottom = (cursor_y + 1..rows.len()).find(|&index| settle::separator(&rows[index]))?;
    let mut prompt = rows[top + 1].chars();
    if !matches!(prompt.next(), Some('❯' | '>')) || !prompt.next().is_none_or(char::is_whitespace)
    {
        return None;
    }
    let text = rows[top + 1..bottom]
        .iter()
        .enumerate()
        .map(|(n, row)| {
            let row: String = if n == 0 {
                row.chars().skip(2).collect()
            } else {
                row.strip_prefix("  ").unwrap_or(row).to_owned()
            };
            row.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(text.trim().to_owned())
}

/// Kimi Code 2.x's boxed composer: ` │ > first` and ` │   more` rows inside
/// its border, from the prompt row at or above the cursor to the bottom edge.
fn kimi_text(screen: &str, cursor_y: usize) -> Option<String> {
    let rows: Vec<String> = screen
        .lines()
        .map(|row| styled_row(row).iter().map(|(ch, _)| ch).collect())
        .collect();
    let inner = |row: &str| -> Option<String> {
        let body = row.strip_prefix(" │")?;
        let end = body.rfind('│')?;
        Some(body[..end].to_owned())
    };
    let start = (0..=cursor_y.min(rows.len().checked_sub(1)?))
        .rev()
        .take_while(|&index| inner(&rows[index]).is_some())
        .find(|&index| inner(&rows[index]).is_some_and(|body| body.starts_with(" > ")))?;
    let mut lines = vec![inner(&rows[start])?[3..].trim_end().to_owned()];
    for row in &rows[start + 1..] {
        if row.starts_with(" ╰") {
            return Some(lines.join("\n").trim().to_owned());
        }
        let body = inner(row)?;
        lines.push(body.strip_prefix("   ").unwrap_or(&body).trim_end().to_owned());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(agent: &str) -> Value {
        json!({"agent": agent})
    }

    #[test]
    fn each_agent_reads_its_residue_or_its_empty_prompt() {
        let rule = "─".repeat(40);
        let claude = format!("● done\n{rule}\n❯ [Image #1] [Image #2][Pasted text #3 +33 lines]\n{rule}\n  paste again to expand\n");
        assert_eq!(read(&record("claude"), &claude, 50, 2), Composer::Text("[Image #1] [Image #2][Pasted text #3 +33 lines]".into()));
        let empty = format!("● done\n{rule}\n❯ \n{rule}\n  ⏵⏵ bypass permissions on\n");
        assert_eq!(read(&record("claude"), &empty, 2, 2), Composer::Empty);
        // Codex 0.167.1 as captured with styles: its prompt glyph is bold.
        let codex = "\x1b[1m›\x1b[0m [Pasted Content 2684 chars]\n\n  GPT-6.1-Sol default · /fixture\n  ? for shortcuts";
        assert_eq!(read(&record("codex"), codex, 30, 0), Composer::Text("[Pasted Content 2684 chars]".into()));
        let kimi = " ╭──────────────────────────────╮\n │ > [paste #1 +36 lines]       │\n │   second line                │\n ╰──────────────────────────────╯\n Never Ask  fixture";
        assert_eq!(read(&record("kimi"), kimi, 26, 1), Composer::Text("[paste #1 +36 lines]\nsecond line".into()));
        let kimi_empty = " ╭──────────────────────────────╮\n │ >                            │\n ╰──────────────────────────────╯\n Never Ask  fixture";
        assert_eq!(read(&record("kimi"), kimi_empty, 5, 1), Composer::Empty);
    }

    #[test]
    fn claude_text_is_read_only_between_its_composer_rules() {
        let rule = "─".repeat(40);
        // A past prompt in history above a picker that hides the composer: no
        // rule surrounds the cursor, so nothing is read (zerus-wkj9l.3).
        let picker = "❯ Ask questions with the tool now.\n● Which one?\n  1. Yes\n  2. No\n";
        assert_eq!(read(&record("claude"), picker, 3, 2), Composer::Unknown);
        // A rule above the cursor but none below is no composer either.
        let open = format!("● done\n{rule}\n❯ 1. Yes\n  2. No\n");
        assert_eq!(read(&record("claude"), &open, 4, 2), Composer::Unknown);
        // The history prompt above is never taken; the composer below is.
        let both = format!(
            "❯ old prompt\n● done\n{rule}\n❯ new text\n  second line\n{rule}\n  ? for shortcuts\n"
        );
        assert_eq!(
            read(&record("claude"), &both, 13, 4),
            Composer::Text("new text\nsecond line".into())
        );
        // A cursor below the composer's bottom rule reads nothing.
        assert_eq!(read(&record("claude"), &both, 4, 6), Composer::Unknown);
        // The row under the top rule must be the prompt itself.
        let other = format!("{rule}\n  continuation without a prompt\n{rule}\n");
        assert_eq!(read(&record("claude"), &other, 4, 1), Composer::Unknown);
    }

    #[test]
    fn an_unrecognized_layout_or_agent_is_never_clearable() {
        assert_eq!(
            read(
                &record("claude"),
                "Do you trust this folder?\n 1. Yes\n",
                3,
                1
            ),
            Composer::Unknown
        );
        assert_eq!(read(&record("opencode"), "❯ text", 6, 0), Composer::Unknown);
        // A Kimi box without its bottom edge is not a composer.
        assert_eq!(
            read(&record("kimi"), " │ > text │\n", 9, 0),
            Composer::Unknown
        );
    }
}
