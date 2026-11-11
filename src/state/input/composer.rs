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
        "claude" => settle::composer_text(agent, screen, cursor_y),
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
    fn an_unrecognized_layout_or_agent_is_never_clearable() {
        assert_eq!(read(&record("claude"), "Do you trust this folder?\n 1. Yes\n", 3, 1), Composer::Unknown);
        assert_eq!(read(&record("opencode"), "❯ text", 6, 0), Composer::Unknown);
        // A Kimi box without its bottom edge is not a composer.
        assert_eq!(read(&record("kimi"), " │ > text │\n", 9, 0), Composer::Unknown);
    }
}
