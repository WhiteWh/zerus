//! Shell quoting for copied and displayed `hgs` commands: a port of the Qt
//! GUI's `SessionCommands` (`tray/src/SessionCommands.h`) and
//! `WindowsCommandLine` (`tray/src/WindowsCommandLine.h`), so a copied command
//! addresses the same CLI and session as the terminal action.
use crate::paths;
use serde::{Deserialize, Serialize};

/// The shell a copied command is written for: POSIX sh on Unix, PowerShell
/// (the Windows Terminal default) on Windows. A parameter rather than a
/// `cfg` in the formatting, so both forms are tested on every platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shell {
    Posix,
    PowerShell,
}

/// PowerShell on Windows, POSIX elsewhere (`SessionCommands::currentShell`).
pub fn current_shell() -> Shell {
    if cfg!(windows) {
        Shell::PowerShell
    } else {
        Shell::Posix
    }
}

/// Whether `text` may go to the clipboard (`ClipboardBackend::isCopyable`): a
/// newline would make the shell run the command the moment it is pasted.
pub fn copyable(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("nothing to copy: the command is empty".to_owned());
    }
    if text.contains(['\n', '\r']) {
        return Err("the command has a newline in it -- copying that is dangerous: the shell would run it the moment you paste".to_owned());
    }
    Ok(())
}

fn all_in(value: &str, allowed: impl Fn(char) -> bool) -> bool {
    !value.is_empty() && value.chars().all(allowed)
}

/// A POSIX sh word: bare when it only has safe characters, else single-quoted.
/// The person may paste it into fish, which reads `\\` and `\'` even inside
/// single quotes, so every backslash and quote stays outside them
/// (`'a'\\'b'`); POSIX shells read that identically (zerus-sa9qn.43).
pub fn shell_quote(value: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if all_in(value, safe) {
        return value.to_owned();
    }
    let mut quoted = String::from("'");
    for c in value.chars() {
        match c {
            '\'' => quoted.push_str(r"'\''"),
            '\\' => quoted.push_str(r"'\\'"),
            c => quoted.push(c),
        }
    }
    quoted.push('\'');
    quoted
}

/// How fish reads one command line, modelled on fish's documented quoting
/// (fish is not installed on the build machines; zerus-sa9qn.43): inside
/// single quotes only `\'` and `\\` are escapes and every other character is
/// literal; outside quotes a backslash escapes the next character, whitespace
/// separates words, and anything fish would act on (`;`, `&`, `|`, `<`, `>`,
/// `(`, `)`, `$`, `*`, `?`, `{`, `}`, a double quote, a newline, or `~` and
/// `#` at the start of a word) is reported instead of read. The same model
/// as hgs's `platform::fish_commands`.
#[cfg(test)]
pub(crate) fn fish_words(line: &str) -> Result<Vec<String>, String> {
    let (mut words, mut word, mut started) = (Vec::new(), String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if started {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
                continue;
            }
            '\'' => loop {
                match chars.next() {
                    None => return Err(format!("an unterminated quote in {line:?}")),
                    Some('\'') => break,
                    Some('\\') if matches!(chars.peek(), Some('\'' | '\\')) => {
                        word.push(chars.next().expect("peeked"));
                    }
                    Some(c) => word.push(c),
                }
            },
            '\\' => match chars.next() {
                Some(
                    c @ ('a'
                    | 'b'
                    | 'e'
                    | 'f'
                    | 'n'
                    | 'r'
                    | 't'
                    | 'v'
                    | 'x'
                    | 'X'
                    | 'u'
                    | 'U'
                    | 'c'
                    | '0'..='7'),
                ) => return Err(format!("the escape \\{c} in {line:?}")),
                Some(c) => word.push(c),
                None => return Err(format!("a trailing backslash in {line:?}")),
            },
            '~' | '#' if !started => return Err(format!("{c:?} starts a word in {line:?}")),
            ';' | '&' | '|' | '<' | '>' | '(' | ')' | '$' | '*' | '?' | '{' | '}' | '"' | '\n' => {
                return Err(format!("fish acts on {c:?} in {line:?}"))
            }
            c => word.push(c),
        }
        started = true;
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// A PowerShell literal: single quotes, with every quote PowerShell accepts as
/// a single quote doubled. Bare words exclude `@` (splatting), commas, `$`,
/// backticks, parentheses and braces.
pub fn power_shell_quote(value: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_./\\:=+-".contains(c);
    if all_in(value, safe) {
        return value.to_owned();
    }
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('\'');
    for c in value.chars() {
        escaped.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            escaped.push(c);
        }
    }
    escaped.push('\'');
    escaped
}

/// Windows PowerShell 5.1 puts a native argument with a space in double quotes
/// without escaping quotes or a final backslash in it, and drops an empty one;
/// PowerShell 7.3 and later pass such arguments correctly, so no quoting suits
/// both (`powerShellPassesUnchanged`). A space is any character its
/// `NeedQuotes` check finds with `char.IsWhiteSpace`: every Unicode
/// White_Space character, as `char::is_whitespace` (zerus-sa9qn.46), and
/// U+180E, which was one before Unicode 6.3 and may still be in the .NET
/// Framework tables 5.1 runs on. Counting it only refuses a rare word.
pub(crate) fn power_shell_passes_unchanged(value: &str) -> bool {
    if value.is_empty() || value.contains('"') {
        return false;
    }
    let spaced = value.chars().any(|c| c.is_whitespace() || c == '\u{180e}');
    !(spaced && value.ends_with('\\'))
}

/// One argument as `QProcess` writes it into a Windows command line
/// (`WindowsCommandLine::argument`): a quote gets a backslash and doubles the
/// backslashes before it; an empty value or one with a space or tab is quoted,
/// its trailing backslashes outside the closing quote.
fn windows_argument(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    let mut backslashes = 0usize;
    for c in value.chars() {
        if c == '"' {
            escaped.extend(std::iter::repeat_n('\\', backslashes + 1));
        }
        backslashes = if c == '\\' { backslashes + 1 } else { 0 };
        escaped.push(c);
    }
    if !(escaped.is_empty() || escaped.contains([' ', '\t'])) {
        return escaped;
    }
    let kept = escaped.trim_end_matches('\\').len();
    format!("\"{}\"{}", &escaped[..kept], &escaped[kept..])
}

/// `WindowsCommandLine::build` with an empty program: every argument is
/// preceded by a space.
fn windows_command_line(arguments: &[String]) -> String {
    arguments
        .iter()
        .map(|argument| format!(" {}", windows_argument(argument)))
        .collect()
}

/// The arguments of a Windows command line, under the rules `windows_argument`
/// writes for: backslashes count only before a quote, and quotes group. `None`
/// for a quote that never closes, which is no argument list a person meant.
pub fn windows_split_arguments(line: &str) -> Option<Vec<String>> {
    let mut arguments = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        if chars.peek().is_none() {
            return Some(arguments);
        }
        let (mut argument, mut quoted) = (String::new(), false);
        while let Some(&c) = chars.peek() {
            if !quoted && matches!(c, ' ' | '\t') {
                break;
            }
            chars.next();
            match c {
                '\\' => {
                    let mut backslashes = 1;
                    while chars.next_if_eq(&'\\').is_some() {
                        backslashes += 1;
                    }
                    let before_quote = chars.peek() == Some(&'"');
                    let kept = if before_quote {
                        backslashes / 2
                    } else {
                        backslashes
                    };
                    argument.extend(std::iter::repeat_n('\\', kept));
                    if before_quote && backslashes % 2 == 1 {
                        chars.next();
                        argument.push('"');
                    }
                }
                '"' => quoted = !quoted,
                other => argument.push(other),
            }
        }
        if quoted {
            return None;
        }
        arguments.push(argument);
    }
}

/// The `CreateProcess` command line of `program` and `arguments` as `QProcess`
/// builds it (`WindowsCommandLine::build`): the program with backslashes, in
/// quotes when it has a space, then a space before each argument (so two
/// after the program).
pub fn windows_program_line(program: &str, arguments: &[String]) -> String {
    let mut line = String::new();
    if !program.is_empty() {
        let quoted = !program.starts_with('"') && !program.ends_with('"') && program.contains(' ');
        let name = if quoted {
            format!("\"{program}\"")
        } else {
            program.to_owned()
        };
        line = name.replace('/', "\\") + " ";
    }
    line + &windows_command_line(arguments)
}

/// The command line for `hgs` with `arguments` in `shell`; an empty `hgs`
/// means the bare name found on PATH.
pub fn command(hgs: &str, arguments: &[String], shell: Shell) -> String {
    let program = if hgs.is_empty() { "hgs" } else { hgs };
    if shell == Shell::PowerShell {
        let quoted = power_shell_quote(&program.replace('/', "\\"));
        // Start-Process hands the exact Windows command line to the program in
        // every PowerShell edition.
        if !arguments.iter().all(|a| power_shell_passes_unchanged(a)) {
            let line = windows_command_line(arguments);
            let line = line.strip_prefix(' ').unwrap_or(&line);
            return format!(
                "Start-Process -FilePath {quoted} -ArgumentList {} -NoNewWindow -Wait",
                power_shell_quote(line)
            );
        }
        // A quoted program needs the call operator to run.
        let mut result = if quoted.starts_with('\'') {
            format!("& {quoted}")
        } else {
            quoted
        };
        for argument in arguments {
            result.push(' ');
            result.push_str(&power_shell_quote(argument));
        }
        return result;
    }
    let mut result = shell_quote(program);
    for argument in arguments {
        result.push(' ');
        result.push_str(&shell_quote(argument));
    }
    result
}

/// Arguments of `hgs [@computer] a NAME`, or of resuming an archived
/// conversation (`SessionCommands::openArguments`).
pub fn open_arguments(computer: &str, name: &str, archive_id: Option<&str>) -> Vec<String> {
    let mut arguments = Vec::new();
    if !computer.is_empty() {
        arguments.push(format!("@{computer}"));
    }
    match archive_id.filter(|id| !id.is_empty()) {
        None => arguments.extend(["a".to_owned(), name.to_owned()]),
        Some(id) => arguments.extend([
            "resume".to_owned(),
            name.to_owned(),
            "--archive".to_owned(),
            id.to_owned(),
        ]),
    }
    arguments
}

/// The command that opens (or resumes) a session (`SessionCommands::open`).
pub fn open_command(
    hgs: &str,
    computer: &str,
    name: &str,
    archive_id: Option<&str>,
    shell: Shell,
) -> String {
    command(hgs, &open_arguments(computer, name, archive_id), shell)
}

/// The command line of a shell in `directory` (`SessionCommands::folderShell`):
/// on this computer a login shell there, or PowerShell's `Set-Location` on
/// Windows; on a peer, which is POSIX, `hgs machine ssh` into it. None when
/// the path is no absolute folder of that computer.
pub fn folder_shell(hgs: &str, computer: &str, directory: &str, shell: Shell) -> Option<String> {
    let windows = computer.is_empty() && shell == Shell::PowerShell;
    let style = if windows {
        paths::Style::Windows
    } else {
        paths::Style::Posix
    };
    if !paths::absolute(directory, style) || directory.contains('\0') {
        return None;
    }
    if windows {
        return Some(format!(
            "Set-Location -LiteralPath {}",
            power_shell_quote(&paths::native(directory, style))
        ));
    }
    if computer.is_empty() {
        return Some(format!(
            "cd -- {} && exec \"${{SHELL:-/bin/sh}}\" -l",
            shell_quote(directory)
        ));
    }
    let arguments = ["machine", "ssh", computer, "--directory", directory].map(str::to_owned);
    Some(command(hgs, &arguments, shell))
}

/// An AppleScript string literal's contents: backslash first, then quote and
/// newline (`MacBackend.cpp:7-23`).
pub fn apple_script_escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Opens `command` in a new Terminal.app window and brings it to the front
/// (`MacBackend::terminalAppScript`).
pub fn terminal_app_script(command: &str) -> String {
    format!(
        "tell application \"Terminal\"\n    do script \"{}\"\n    activate\nend tell",
        apple_script_escape(command)
    )
}

/// Qt's tray message on macOS (`MacBackend::notificationScript`).
pub fn notification_script(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        apple_script_escape(body),
        apple_script_escape(title)
    )
}

/// A copied command as its notice shows it: at most 120 characters, cut in
/// the middle (`ClipboardBackend::notificationText`).
pub fn notification_text(command: &str) -> String {
    const MAX: usize = 120;
    const ELLIPSIS: &str = " \u{2026} ";
    let chars: Vec<char> = command.chars().collect();
    if chars.len() <= MAX {
        return command.to_owned();
    }
    let keep = MAX - ELLIPSIS.chars().count();
    let (head, tail) = (keep / 2, keep - keep / 2);
    let mut shown: String = chars[..head].iter().collect();
    shown.push_str(ELLIPSIS);
    shown.extend(&chars[chars.len() - tail..]);
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use Shell::{Posix, PowerShell};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn open(hgs: &str, computer: &str, name: &str, archive: Option<&str>, shell: Shell) -> String {
        open_command(hgs, computer, name, archive, shell)
    }

    /// What Windows PowerShell 5.1's `NeedQuotes` counts as a space
    /// (`char.IsWhiteSpace`): the Unicode White_Space characters.
    const POWER_SHELL_SPACES: &[char] = &[
        '\t', '\n', '\u{b}', '\u{c}', '\r', ' ', '\u{85}', '\u{a0}', '\u{1680}', '\u{2000}',
        '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
        '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}',
        '\u{3000}',
    ];

    /// zerus-sa9qn.46: PowerShell 5.1 wraps a word with any of these spaces in
    /// `"…"`, where a final backslash escapes the closing quote and the
    /// following arguments merge into the word; with no final backslash it
    /// reaches the program whole.
    #[test]
    fn power_shell_5_1_spacing_is_every_unicode_space() {
        let white_space: Vec<char> = (0..=0x10ffff)
            .filter_map(char::from_u32)
            .filter(|c| c.is_whitespace())
            .collect();
        assert_eq!(white_space, POWER_SHELL_SPACES);
        for &space in POWER_SHELL_SPACES {
            let code = space as u32;
            assert!(
                !power_shell_passes_unchanged(&format!("ws{space}x\\")),
                "U+{code:04X}"
            );
            assert!(
                !power_shell_passes_unchanged(&format!("{space}\\")),
                "U+{code:04X}"
            );
            assert!(
                power_shell_passes_unchanged(&format!("ws{space}x")),
                "U+{code:04X}"
            );
            assert!(
                power_shell_passes_unchanged(&format!("ws{space}x\\y")),
                "U+{code:04X}"
            );
        }
    }

    /// Spaces PowerShell 5.1 may count on top of today's White_Space: U+180E
    /// was a space separator (Zs) before Unicode 6.3, and the .NET Framework
    /// tables behind its `char.IsWhiteSpace` may predate that.
    const EARLIER_POWER_SHELL_SPACES: &[char] = &['\u{180e}'];

    #[test]
    fn power_shell_5_1_spacing_also_counts_spaces_of_earlier_unicode() {
        for &space in EARLIER_POWER_SHELL_SPACES {
            let code = space as u32;
            assert!(!space.is_whitespace(), "U+{code:04X}");
            assert!(
                !power_shell_passes_unchanged(&format!("ws{space}x\\")),
                "U+{code:04X}"
            );
            assert!(
                power_shell_passes_unchanged(&format!("ws{space}x")),
                "U+{code:04X}"
            );
        }
    }

    /// A zero-width character is no space to `char.IsWhiteSpace`, so
    /// PowerShell 5.1 passes such a word bare, its final backslash included.
    #[test]
    fn a_zero_width_character_is_not_power_shell_spacing() {
        for zero_width in ['\u{200b}', '\u{200c}', '\u{200d}', '\u{2060}', '\u{feff}'] {
            assert!(!zero_width.is_whitespace());
            let word = format!("ws{zero_width}x\\");
            assert!(power_shell_passes_unchanged(&word), "{word:?}");
        }
    }

    #[test]
    fn windows_arguments_split_back_into_what_was_written() {
        for arguments in [
            vec![],
            vec!["--background".to_owned()],
            vec!["--hgs".to_owned(), r"C:\Program Files\z\hgs.exe".to_owned()],
            vec![r"C:\trailing\".to_owned(), r"C:\with space\".to_owned()],
            vec![String::new(), "a".to_owned(), String::new()],
            vec![
                r#"say "hi""#.to_owned(),
                r#"back\"quote"#.to_owned(),
                "tab\there".to_owned(),
            ],
        ] {
            assert_eq!(
                windows_split_arguments(&windows_command_line(&arguments)).as_ref(),
                Some(&arguments),
                "{arguments:?}"
            );
        }
    }

    #[test]
    fn an_unterminated_windows_quote_is_no_argument_list() {
        assert_eq!(windows_split_arguments(r#" --hgs "C:\no end"#), None);
        assert_eq!(windows_split_arguments(r#" ""#), None);
    }

    /// Words a person pastes, from session names and folders; `x\'; touch
    /// pwned; #` is the #2127 review's folder name.
    const HOSTILE: &[&str] = &[
        "claude/p/x\\'; touch pwned; #",
        "a\\b",
        "x\\",
        "\\'",
        "\\\\'",
        "it's",
        "$(touch pwned)",
        "`touch pwned`",
        "a\"b; touch pwned",
        "x;touch pwned",
        "x\u{2019}; touch pwned",
        "",
    ];

    /// zerus-sa9qn.43: a copied session command reads back exactly in fish
    /// (modelled, `fish_words`) and in sh (run).
    #[test]
    fn copied_posix_commands_read_back_exactly_in_fish_and_sh() {
        for word in HOSTILE.iter().copied() {
            assert_eq!(
                fish_words(&shell_quote(word)),
                Ok(vec![word.to_owned()]),
                "{word:?}"
            );
        }
        let hgs = "/home/example/.local/bin/hgs";
        for (index, name) in HOSTILE.iter().copied().enumerate() {
            let line = open_command(hgs, "mac\\'", name, None, Shell::Posix);
            let mut words = vec![hgs.to_owned()];
            words.extend(open_arguments("mac\\'", name, None));
            assert_eq!(fish_words(&line), Ok(words.clone()), "{line}");
            #[cfg(unix)]
            {
                // The program word is plain, so sh can run a stand-in instead.
                let script = format!(
                    "run() {{ printf '%s\\0' \"$@\"; }}\n{}",
                    line.strip_prefix(hgs)
                        .map(|rest| format!("run{rest}"))
                        .unwrap()
                );
                let folder = std::env::temp_dir()
                    .join(format!("zerus-shell-fish-{}-{index}", std::process::id()));
                let _ = std::fs::remove_dir_all(&folder);
                std::fs::create_dir_all(&folder).unwrap();
                let out = std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg(script)
                    .current_dir(&folder)
                    .env("PATH", "/usr/bin:/bin")
                    .output()
                    .unwrap();
                let ran = folder.join("pwned").exists();
                std::fs::remove_dir_all(&folder).unwrap();
                assert!(!ran, "{line}");
                let read: Vec<String> = String::from_utf8(out.stdout)
                    .unwrap()
                    .split_terminator('\0')
                    .map(str::to_owned)
                    .collect();
                assert_eq!(read, words[1..], "{line}");
            }
        }
    }

    #[test]
    fn posix_quotes_only_what_needs_it() {
        assert_eq!(
            shell_quote("claude/sample-project/review"),
            "claude/sample-project/review"
        );
        assert_eq!(shell_quote("release planning"), "'release planning'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(
            open(
                "/home/example/.local/bin/hgs",
                "mac",
                "claude/sample-project/review",
                None,
                Posix
            ),
            "/home/example/.local/bin/hgs @mac a claude/sample-project/review"
        );
        assert_eq!(
            open("hgs", "", "claude/sample-project/review", Some("0f3c2b1a-1111-4222-8333-944445555666"), Posix),
            "hgs resume claude/sample-project/review --archive 0f3c2b1a-1111-4222-8333-944445555666"
        );
    }

    #[test]
    fn power_shell_uses_the_call_operator_or_start_process() {
        assert_eq!(
            command(
                "C:/Program Files/Zerus/hgs.exe",
                &args(&["a", "claude/sample-project/review"]),
                PowerShell
            ),
            "& 'C:\\Program Files\\Zerus\\hgs.exe' a claude/sample-project/review"
        );
        assert_eq!(power_shell_quote("it\u{2019}s"), "'it\u{2019}\u{2019}s'");
        assert!(command(
            "hgs",
            &args(&["a", "claude/sample-project/release notes\\"]),
            PowerShell
        )
        .starts_with("Start-Process -FilePath hgs -ArgumentList "));
    }

    // Ports of test_terminalbackend.cpp.
    #[test]
    fn session_commands_preserve_identity_and_quoting() {
        assert_eq!(
            open("", "", "codex/docs/review", None, Posix),
            "hgs a codex/docs/review"
        );
        assert_eq!(
            open("/opt/hgs", "mac", "codex/проект/Новый план", None, Posix),
            "/opt/hgs @mac a 'codex/проект/Новый план'"
        );
        assert_eq!(
            open(
                "/tmp/hgs's bin/hgs",
                "mac",
                "codex/docs/it's $(exit 7); `exit 8`",
                None,
                Posix
            ),
            "'/tmp/hgs'\\''s bin/hgs' @mac a 'codex/docs/it'\\''s $(exit 7); `exit 8`'"
        );
        assert_eq!(
            command(
                "",
                &args(&["codex", "/work/a project", "--new", "-n", "plan"]),
                Posix
            ),
            "hgs codex '/work/a project' --new -n plan"
        );
    }

    #[test]
    fn archive_commands_restore_the_selected_instance() {
        assert_eq!(
            open(
                "/usr/local/bin/hgs",
                "mac",
                "claude/infra/review",
                Some("archive-one"),
                Posix
            ),
            "/usr/local/bin/hgs @mac resume claude/infra/review --archive archive-one"
        );
        assert_ne!(
            open("", "", "codex/docs/review", Some("older"), Posix),
            open("", "", "codex/docs/review", Some("newer"), Posix)
        );
        assert_eq!(
            open("", "", "codex/docs/review", Some("id with ' quotes"), Posix),
            "hgs resume codex/docs/review --archive 'id with '\\'' quotes'"
        );
    }

    #[test]
    fn power_shell_commands_quote_every_non_plain_argument() {
        assert_eq!(
            open(
                "C:/Program Files/Zerus/hgs.exe",
                "mac",
                "codex/проект/Новый план",
                None,
                PowerShell
            ),
            "& 'C:\\Program Files\\Zerus\\hgs.exe' '@mac' a 'codex/проект/Новый план'"
        );
        assert_eq!(
            open(
                "C:\\Zerus\\hgs.exe",
                "",
                "codex/docs/review",
                None,
                PowerShell
            ),
            "C:\\Zerus\\hgs.exe a codex/docs/review"
        );
        assert_eq!(
            command(
                "",
                &args(&[
                    "it's",
                    "$HOME",
                    "`x`",
                    "a,b",
                    "(1)",
                    "50%",
                    "C:\\dir\\",
                    "--new",
                    "-n"
                ]),
                PowerShell
            ),
            "hgs 'it''s' '$HOME' '`x`' 'a,b' '(1)' '50%' C:\\dir\\ --new -n"
        );
        assert_eq!(
            open(
                "C:/Program Files/Zerus/hgs.exe",
                "mac",
                "claude/p/fix \"login\" it's",
                None,
                PowerShell
            ),
            "Start-Process -FilePath 'C:\\Program Files\\Zerus\\hgs.exe' \
             -ArgumentList '@mac a \"claude/p/fix \\\"login\\\" it''s\"' -NoNewWindow -Wait"
        );
        assert_eq!(
            command("", &args(&["codex", "/work/my dir\\", "-n", ""]), PowerShell),
            "Start-Process -FilePath hgs -ArgumentList 'codex \"/work/my dir\"\\ -n \"\"' -NoNewWindow -Wait"
        );
        assert_eq!(power_shell_quote("don\u{2019}t"), "'don\u{2019}\u{2019}t'");
        assert_eq!(
            open("", "", "codex/p", Some("id with ' quotes"), PowerShell),
            "hgs resume codex/p --archive 'id with '' quotes'"
        );
    }

    #[test]
    fn open_arguments_put_the_computer_first() {
        assert_eq!(
            open_arguments("mac", "codex/p", None),
            args(&["@mac", "a", "codex/p"])
        );
        assert_eq!(
            open_arguments("", "codex/p", Some("old")),
            args(&["resume", "codex/p", "--archive", "old"])
        );
    }

    // Ports of test_windowsdesktop.cpp.
    #[test]
    fn template_program_command_is_one_powershell_line() {
        assert_eq!(
            command(
                "C:/Zerus/hgs.exe",
                &args(&["@mac", "a", "codex/p"]),
                PowerShell
            ),
            "C:\\Zerus\\hgs.exe '@mac' a codex/p"
        );
    }

    // Port of test_terminalview.cpp (the program part is empty here).
    #[test]
    fn windows_command_line_matches_qprocess_quoting() {
        assert_eq!(
            windows_command_line(&args(&["@mac", "a", "codex/p/my name"])),
            " @mac a \"codex/p/my name\""
        );
        assert_eq!(
            windows_command_line(&args(&[
                "",
                "say \"hi\"",
                "C:\\dir\\",
                "C:\\my dir\\",
                "a\\\"b"
            ])),
            " \"\" \"say \\\"hi\\\"\" C:\\dir\\ \"C:\\my dir\"\\ a\\\\\\\"b"
        );
    }

    #[test]
    fn current_shell_follows_the_platform() {
        assert_eq!(
            current_shell(),
            if cfg!(windows) { PowerShell } else { Posix }
        );
    }

    #[test]
    fn copyable_text_refuses_what_qt_refuses() {
        assert_eq!(copyable("hgs a x"), Ok(()));
        assert_eq!(
            copyable(" \t "),
            Err("nothing to copy: the command is empty".to_owned())
        );
        assert_eq!(
            copyable(""),
            Err("nothing to copy: the command is empty".to_owned())
        );
        let newline = "the command has a newline in it -- copying that is dangerous: the shell would run it the moment you paste";
        assert_eq!(copyable("a\nb"), Err(newline.to_owned()));
        assert_eq!(copyable("a\rb"), Err(newline.to_owned()));
        assert_eq!(copyable("a\n"), Err(newline.to_owned()));
    }

    #[test]
    fn apple_script_strings_escape_backslash_first() {
        assert_eq!(apple_script_escape(r#"a\b"c"#), r#"a\\b\"c"#);
        assert_eq!(apple_script_escape("one\ntwo"), "one\\ntwo");
        assert_eq!(
            terminal_app_script("hgs @mac a 'claude/R\"D'"),
            "tell application \"Terminal\"\n    do script \"hgs @mac a 'claude/R\\\"D'\"\n    activate\nend tell"
        );
        assert_eq!(
            notification_script("Zerus: copied", "hgs a x\nnext"),
            "display notification \"hgs a x\\nnext\" with title \"Zerus: copied\""
        );
    }

    #[test]
    fn long_copied_commands_are_cut_in_the_middle() {
        assert_eq!(notification_text("hgs a x"), "hgs a x");
        let long = format!("hgs @mac a {}", "x".repeat(200));
        let shown = notification_text(&long);
        assert_eq!(shown.chars().count(), 120);
        assert!(shown.starts_with("hgs @mac a xx"));
        assert!(shown.contains(" \u{2026} "));
    }
}
