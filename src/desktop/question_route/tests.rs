/// Every literal a `source` takes under `src/state`, classified on purpose
/// (zerus-sa9qn.8). Only Claude's own AskUserQuestion hook card routes; a new
/// literal fails [`every_source_literal_under_state_is_classified`] until it
/// is classified here.
mod inventory {
    use std::{collections::BTreeMap, path::Path};
    use zerus_client_core::desktop::question_route::{routable, ROUTED_SOURCE};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Class {
        /// Claude's own AskUserQuestion of the main agent, from its
        /// PreToolUse hook (`questions::from_hook`): the only route.
        Routes,
        /// Claude Code's, Codex's or Kimi's own prompt, or a synthetic
        /// native card: trust, approvals, sign-in, limits, resumes, choices.
        NativePrompt,
        /// Codex async questions: optional, never blocking. Codex therefore
        /// has nothing to route in v1.
        Optional,
        /// Kimi, DeepSeek and OpenCode questions: zerus-sa9qn.34.
        OtherProvider,
        /// Not a question card's source: history, processes, links, goals,
        /// usage, tool metadata.
        NotACard,
    }
    use Class::*;
    const CLASSES: &[(&str, Class)] = &[
        ("hook", Routes),
        // WhiteWh fork: its own native cards, and sources that are no card
        // (teams, builds, model servers, usage, session state).
        ("claude_bypass_warning", NativePrompt),
        ("codex_tool_approval", NativePrompt),
        ("CASE WHEN NEW.members_rev IS NEW.revision THEN NEW.members ELSE json_extract(NEW.payload,'$.members') END", NotACard),
        ("Zerus safe-call rule", NotACard),
        ("api", NotACard),
        ("builtin", NotACard),
        ("catalog", NotACard),
        ("compact", NotACard),
        ("default", NotACard),
        ("dsh", NotACard),
        ("dsh-profile", NotACard),
        ("engine", NotACard),
        ("hgs_run", NotACard),
        ("owner", NotACard),
        ("project", NotACard),
        ("safe-action shape", NotACard),
        ("settings", NotACard),
        ("team lead dsh/x/lead", NotACard),
        ("team_run", NotACard),
        ("team_stage", NotACard),
        ("claude_native_question", NativePrompt),
        ("claude_tool_approval", NativePrompt),
        ("claude_permission_mode", NativePrompt),
        ("claude_terminal_safety", NativePrompt),
        ("claude_terminal_choice", NativePrompt),
        ("claude_resume", NativePrompt),
        ("claude_usage_limit", NativePrompt),
        ("claude_folder_trust", NativePrompt),
        ("codex_hooks_trust", NativePrompt),
        ("codex_update", NativePrompt),
        ("codex_rate_limit", NativePrompt),
        ("codex_goal_resume", NativePrompt),
        ("codex_folder_trust", NativePrompt),
        ("kimi_folder_trust", NativePrompt),
        ("kimi_cache_hint", NativePrompt),
        ("startup_terminal_choice", NativePrompt),
        ("codex_async", Optional),
        ("kimi_wire", OtherProvider),
        ("kimi_plan_review", OtherProvider),
        ("kimi_tool_approval", OtherProvider),
        ("dsh_native", OtherProvider),
        ("dsh_native_api", OtherProvider),
        ("opencode_native", OtherProvider),
        ("adopt", NotACard),
        ("auto", NotACard),
        ("claude", NotACard),
        ("claude_goal_status_v1", NotACard),
        ("claude_transcript", NotACard),
        ("claude_workflow_journal", NotACard),
        ("clear", NotACard),
        ("cli", NotACard),
        ("codex_goals_v1", NotACard),
        ("codex_native_log", NotACard),
        ("codex_native_transcript", NotACard),
        ("codex_transcript", NotACard),
        ("command", NotACard),
        ("dsh_calls", NotACard),
        ("dsh_goal_v1", NotACard),
        ("dsh_jobs", NotACard),
        ("exec", NotACard),
        ("forwarded", NotACard),
        ("fresh_start", NotACard),
        ("hgs_delivery", NotACard),
        ("hgs_voice", NotACard),
        ("hooks", NotACard),
        ("journal", NotACard),
        ("kimi_goal_v1", NotACard),
        ("kimi_session_state", NotACard),
        ("kimi_tasks", NotACard),
        ("launch", NotACard),
        ("model", NotACard),
        ("native", NotACard),
        ("native_chooser", NotACard),
        ("native_compaction", NotACard),
        ("native_footer", NotACard),
        ("native_history", NotACard),
        ("native_hook", NotACard),
        ("native_roster", NotACard),
        ("native_statusline", NotACard),
        ("native_terminal", NotACard),
        ("native_usage", NotACard),
        ("process_tree", NotACard),
        ("provider_hook", NotACard),
        ("read-marks.json", NotACard),
        ("refusal_fallback_preference", NotACard),
        ("resume", NotACard),
        ("saved_binding", NotACard),
        ("snapshot", NotACard),
        ("startup", NotACard),
        ("subagent_summary", NotACard),
    ];

    /// The fifth argument of each `card(...)` call: `questions::card`'s
    /// source.
    fn card_sources(text: &str, found: &mut Vec<String>) {
        let call = regex::Regex::new(r"(?:^|[^A-Za-z0-9_])card\(").unwrap();
        for start in call.find_iter(text) {
            let mut depth = 1;
            let mut args = vec![String::new()];
            for c in text[start.end()..].chars() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    _ => {}
                }
                if depth == 0 {
                    break;
                }
                if depth == 1 && c == ',' {
                    args.push(String::new());
                } else {
                    args.last_mut().unwrap().push(c);
                }
            }
            if let Some(arg) = args.get(4).map(|a| a.trim()) {
                if let Some(literal) = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
                    found.push(literal.into());
                }
            }
        }
    }
    /// Every `source` literal of one file: JSON keys, assignments,
    /// comparisons, `*SOURCE` constants and `card(...)` arguments.
    fn literals(text: &str) -> Vec<String> {
        let patterns = [
            r#""source"\s*:\s*"([^"\\]*)""#,
            r#"\["source"\]\s*=\s*json!\(\s*"([^"\\]*)"\s*\)"#,
            r#"\["source"\]\s*[!=]=\s*(?:json!\(\s*)?"([^"\\]*)""#,
            r#""source"\s*\)\s*[!=]=\s*"([^"\\]*)""#,
            r#"\b[A-Z_]*SOURCE\s*:\s*&str\s*=\s*"([^"\\]*)""#,
        ];
        let mut found = Vec::new();
        for pattern in patterns {
            let pattern = regex::Regex::new(pattern).unwrap();
            found.extend(pattern.captures_iter(text).map(|c| c[1].to_owned()));
        }
        card_sources(text, &mut found);
        found
    }
    fn scan(dir: &Path, found: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                for literal in literals(&text) {
                    found
                        .entry(literal)
                        .or_insert_with(|| path.display().to_string());
                }
            }
        }
    }

    #[test]
    fn the_scanner_finds_each_way_state_names_a_source() {
        let text = r#"json!({"source":"a"}); card["source"] = json!("b"); x["source"]=="c";
            string(card, "source") != "d"; pub(super) const SOURCE: &str = "e";
            const FORWARDED_SOURCE: &str = "f"; card(record, &id, tool, &items, "g", now());
            card(record, id, tool(x), &event["q"], "h", 1.0)"#;
        let mut found = literals(text);
        found.sort();
        assert_eq!(found, ["a", "b", "c", "d", "e", "f", "g", "h"]);
    }

    #[test]
    fn every_source_literal_under_state_is_classified() {
        let mut found = BTreeMap::new();
        scan(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src/state"),
            &mut found,
        );
        assert!(found.contains_key("hook") && found.contains_key("claude_native_question"));
        let classes: BTreeMap<&str, Class> = CLASSES.iter().copied().collect();
        assert_eq!(classes.len(), CLASSES.len(), "one class per literal");
        let unclassified: Vec<_> = found
            .iter()
            .filter(|(literal, _)| !classes.contains_key(literal.as_str()))
            .collect();
        assert!(
            unclassified.is_empty(),
            "classify each new source literal in CLASSES (only Claude's own hook question routes): {unclassified:?}"
        );
    }

    #[test]
    fn only_the_hook_source_routes() {
        let routes: Vec<&str> = CLASSES
            .iter()
            .filter(|(_, class)| *class == Routes)
            .map(|(literal, _)| *literal)
            .collect();
        assert_eq!(routes, [ROUTED_SOURCE]);
        // An otherwise routable card routes exactly for the routed class.
        for (literal, class) in CLASSES {
            let card = serde_json::json!({"source": literal, "agent_id": "main",
                "can_answer": true, "questions": [{"question": "?"}]});
            assert_eq!(routable(&card), *class == Routes, "{literal}");
        }
    }
}

#[cfg(target_os = "linux")]
mod router;
