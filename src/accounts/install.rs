//! Explicit terminal action; catalog/identity reads never invoke an installer.
//!
//! Each agent installs the way its vendor documents for this computer: an npm
//! package with `npm install -g`, otherwise the vendor's installer script for the
//! platform, `install.sh` run by bash on Unix and `install.ps1` run by PowerShell
//! on Windows. Windows never gets the Unix script: its `bash` is WSL's launcher,
//! which would install a Linux agent inside WSL, or Git Bash, where both vendors'
//! `install.sh` refuse to run.
use super::*;
use std::ffi::OsString;

/// How `hgs account install` installs an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Installer {
    /// `npm install -g` of this package.
    Npm(&'static str),
    /// The vendor's installer script at this fixed HTTPS URL.
    Script(&'static str),
}

/// The installer of `provider` on Unix or, with `windows`, on Windows; None for
/// an agent hgs does not install.
fn installer(provider: &str, windows: bool) -> Option<Installer> {
    let script =
        |unix, windows_script| Installer::Script(if windows { windows_script } else { unix });
    Some(match provider {
        "codex" => Installer::Npm("@openai/codex"),
        "dsh" => Installer::Npm("@deepseek-ai/dsh@0.2.2-rc.1"),
        "claude" => script(
            "https://claude.ai/install.sh",
            "https://claude.ai/install.ps1",
        ),
        "kimi" => script(
            "https://code.kimi.com/kimi-code/install.sh",
            "https://code.kimi.com/kimi-code/install.ps1",
        ),
        _ => return None,
    })
}

/// The program `npm install -g` starts, given what `platform::which("npm")`
/// found; None without npm. On Windows that is the file found with PATHEXT,
/// Node.js's `npm.cmd`: given the bare name, std would look for `npm.exe` only.
/// std runs a batch file through `cmd.exe` with its arguments escaped, and these
/// arguments are fixed package names. Elsewhere it is `npm` from PATH.
fn npm_program(windows: bool, found: Option<PathBuf>) -> Option<OsString> {
    let found = found?;
    Some(if windows {
        found.into_os_string()
    } else {
        "npm".into()
    })
}

const DSH_OVERRIDES: [&str; 2] = [
    "overrides.@modelcontextprotocol/client=2.3.0",
    "overrides.fflate=0.8.3",
];

fn parse_npm_root(bytes: &[u8]) -> Result<PathBuf> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::new(1, "npm returned a non-UTF-8 global package root"))?;
    let path = text.trim_end_matches(['\r', '\n']);
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(Error::new(1, "npm returned an invalid global package root"));
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(Error::new(1, "npm returned a relative global package root"));
    }
    Ok(path.join("@deepseek-ai/dsh"))
}

fn dsh_root(npm: &OsString) -> Result<PathBuf> {
    let mut command = Command::new(npm);
    command.args(["root", "-g"]);
    let output = inspection::capture(command, &[], 30).map_err(|error| {
        Error::new(1, format!("Could not locate the DeepSeek package: {error}"))
    })?;
    let root = parse_npm_root(&output)?;
    if !root.is_dir() {
        return Err(Error::new(1, "The installed DeepSeek package is missing"));
    }
    Ok(root)
}

fn dsh_override_arguments(root: &Path) -> [Vec<OsString>; 2] {
    [
        ["pkg", "set", "--prefix"]
            .into_iter()
            .map(OsString::from)
            .chain([root.as_os_str().into()])
            .chain(DSH_OVERRIDES.map(OsString::from))
            .collect(),
        ["install", "--omit=dev", "--prefix"]
            .into_iter()
            .map(OsString::from)
            .chain([root.as_os_str().into()])
            .collect(),
    ]
}

fn apply_dsh_overrides(npm: &OsString, root: &Path) -> Result<i32> {
    for arguments in dsh_override_arguments(root) {
        let code = platform::wait_interactive(Command::new(npm).args(arguments))?;
        if code != 0 {
            return Ok(code);
        }
    }
    Ok(0)
}

/// The command line that runs the downloaded installer `script`: bash on Unix;
/// on Windows `powershell` (`platform::powershell`, pwsh.exe or Windows
/// PowerShell) without profiles, with the execution policy bypassed for this
/// process only, as the vendors' documented `irm … | iex` runs the script.
fn script_command(script: &Path, powershell: Option<&[String]>) -> Vec<OsString> {
    match powershell {
        None => vec!["bash".into(), script.into()],
        Some(powershell) => powershell
            .iter()
            .map(OsString::from)
            .chain(
                ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
                    .into_iter()
                    .map(OsString::from),
            )
            .chain([script.into()])
            .collect(),
    }
}

/// A downloaded `install.ps1` as PowerShell should read it. Windows PowerShell
/// 5.1 decodes a script without a byte order mark in the ANSI code page, which
/// garbles any non-ASCII UTF-8 text, so UTF-8 gains its mark. Anything else (a
/// marked script, or bytes that are not UTF-8) stays as it is.
fn powershell_script(bytes: Vec<u8>) -> Vec<u8> {
    const MARKS: [&[u8]; 3] = [b"\xEF\xBB\xBF", b"\xFF\xFE", b"\xFE\xFF"];
    if MARKS.iter().any(|mark| bytes.starts_with(mark)) || std::str::from_utf8(&bytes).is_err() {
        return bytes;
    }
    [MARKS[0], &bytes].concat()
}

pub(super) fn run(provider: &str, dry: bool) -> Result<i32> {
    let windows = cfg!(windows);
    let installer =
        installer(provider, windows).ok_or_else(|| Error::new(1, "Unknown agent installer."))?;
    let (package, url) = match installer {
        Installer::Npm(package) => (package, ""),
        Installer::Script(url) => ("", url),
    };
    if dry {
        println!(
            "{}",
            json!({"provider":provider,"package":package,"installer_url":url,"dry_run":true})
        );
        return Ok(0);
    }
    println!("Installing {provider} on this machine. After installation, return to Zerus and refresh Accounts.");
    if !package.is_empty() {
        let npm = npm_program(windows, platform::which("npm")).ok_or_else(|| {
            Error::new(
                1,
                "Install Node.js and npm on this machine, then retry Install agent.",
            )
        })?;
        let code = platform::wait_interactive(Command::new(&npm).args(["install", "-g", package]))?;
        if code != 0 || provider != "dsh" {
            return Ok(code);
        }
        let root = dsh_root(&npm)?;
        return apply_dsh_overrides(&npm, &root);
    }
    #[cfg(windows)]
    let powershell = Some(platform::powershell().map_err(|e| {
        Error::new(
            1,
            format!("Could not run the agent installer: {e}. Nothing was downloaded or executed."),
        )
    })?);
    #[cfg(not(windows))]
    let powershell: Option<Vec<String>> = None;
    // Download completely before running, so a failed transfer cannot execute
    // a partial script. Only the provider's fixed HTTPS installer is accepted.
    let script = tempfile::NamedTempFile::new()?;
    let status = platform::status(
        Command::new("curl")
            .args([
                "--fail",
                "--show-error",
                "--silent",
                "--location",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--connect-timeout",
                "10",
                "--max-time",
                "120",
                "--max-filesize",
                "2097152",
                "--output",
            ])
            .arg(script.path())
            .arg(url),
    )?;
    if !status.success() {
        return Err(Error::new(
            1,
            "Could not download the agent installer. Nothing was executed.",
        ));
    }
    // PowerShell runs only a `.ps1` file, and Windows PowerShell opens it only
    // while no other handle may write to it: it gets a closed copy, removed on
    // drop.
    let copy;
    let run = match powershell {
        None => script.path(),
        Some(_) => {
            let mut file = tempfile::Builder::new().suffix(".ps1").tempfile()?;
            file.write_all(&powershell_script(fs::read(script.path())?))?;
            copy = file.into_temp_path();
            &copy
        }
    };
    let command = script_command(run, powershell.as_deref());
    platform::wait_interactive(Command::new(&command[0]).args(&command[1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_agent_installs_with_its_vendor_method_for_the_platform() {
        for windows in [false, true] {
            assert_eq!(
                installer("codex", windows),
                Some(Installer::Npm("@openai/codex"))
            );
            assert_eq!(
                installer("dsh", windows),
                Some(Installer::Npm("@deepseek-ai/dsh@0.2.2-rc.1"))
            );
            for unknown in ["opencode", "sh", "arbitrary-command", ""] {
                assert_eq!(installer(unknown, windows), None);
            }
        }
        assert_eq!(
            installer("claude", false),
            Some(Installer::Script("https://claude.ai/install.sh"))
        );
        assert_eq!(
            installer("kimi", false),
            Some(Installer::Script(
                "https://code.kimi.com/kimi-code/install.sh"
            ))
        );
        // Windows gets the vendors' PowerShell installers, never install.sh.
        assert_eq!(
            installer("claude", true),
            Some(Installer::Script("https://claude.ai/install.ps1"))
        );
        assert_eq!(
            installer("kimi", true),
            Some(Installer::Script(
                "https://code.kimi.com/kimi-code/install.ps1"
            ))
        );
    }

    #[test]
    fn npm_is_the_file_pathext_finds_on_windows_and_npm_elsewhere() {
        let cmd = PathBuf::from(r"C:\Program Files\nodejs\npm.cmd");
        assert_eq!(
            npm_program(true, Some(cmd.clone())),
            Some(OsString::from(r"C:\Program Files\nodejs\npm.cmd"))
        );
        assert_eq!(
            npm_program(false, Some(PathBuf::from("/usr/bin/npm"))),
            Some(OsString::from("npm"))
        );
        assert_eq!(npm_program(true, None), None);
        assert_eq!(npm_program(false, None), None);
    }

    #[test]
    fn deepseek_security_overrides_stay_inside_its_package_and_omit_development_dependencies() {
        let native_root = if cfg!(windows) {
            r"C:\fixture\lib\node_modules"
        } else {
            "/fixture/lib/node_modules"
        };
        let root = parse_npm_root(format!("{native_root}\n").as_bytes()).unwrap();
        assert_eq!(root, Path::new(native_root).join("@deepseek-ai/dsh"));
        let commands = dsh_override_arguments(&root);
        assert_eq!(
            commands[0],
            [
                "pkg".into(),
                "set".into(),
                "--prefix".into(),
                root.clone().into_os_string(),
                DSH_OVERRIDES[0].into(),
                DSH_OVERRIDES[1].into()
            ]
        );
        assert_eq!(
            commands[1],
            [
                "install".into(),
                "--omit=dev".into(),
                "--prefix".into(),
                root.into_os_string()
            ]
        );
        for bytes in [b"".as_slice(), b"relative\n", b"/one\n/two\n", b"/one\0"] {
            assert!(parse_npm_root(bytes).is_err());
        }
    }

    #[test]
    fn bash_runs_the_unix_script_and_powershell_the_windows_one() {
        let words = |command: Vec<OsString>| {
            command
                .into_iter()
                .map(|word| word.into_string().unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            words(script_command(Path::new("/tmp/.tmpAbC123"), None)),
            ["bash", "/tmp/.tmpAbC123"]
        );
        let script = Path::new(r"C:\Users\a\AppData\Local\Temp\.tmpAbC123.ps1");
        let windows_powershell = [
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".to_string(),
            "-NoLogo".into(),
        ];
        assert_eq!(
            words(script_command(script, Some(&windows_powershell))),
            [
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
                "-NoLogo",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                r"C:\Users\a\AppData\Local\Temp\.tmpAbC123.ps1"
            ]
        );
        let pwsh = [r"C:\Program Files\PowerShell\7\pwsh.exe".to_string()];
        assert_eq!(
            words(script_command(script, Some(&pwsh))),
            [
                r"C:\Program Files\PowerShell\7\pwsh.exe",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                r"C:\Users\a\AppData\Local\Temp\.tmpAbC123.ps1"
            ]
        );
    }

    #[test]
    fn a_utf8_powershell_script_gains_its_byte_order_mark_once() {
        let marked = powershell_script(
            "# domain \u{2014} comment\r\nexit 0\r\n"
                .as_bytes()
                .to_vec(),
        );
        assert_eq!(&marked[..3], b"\xEF\xBB\xBF");
        assert_eq!(
            &marked[3..],
            "# domain \u{2014} comment\r\nexit 0\r\n".as_bytes()
        );
        assert_eq!(powershell_script(marked.clone()), marked);
        assert_eq!(
            powershell_script(b"exit 0\n".to_vec()),
            b"\xEF\xBB\xBFexit 0\n"
        );
        // UTF-16 keeps its own mark; bytes that are not UTF-8 stay as they are.
        assert_eq!(powershell_script(b"\xFF\xFEe\0".to_vec()), b"\xFF\xFEe\0");
        assert_eq!(powershell_script(b"caf\xE9".to_vec()), b"caf\xE9");
        assert_eq!(powershell_script(Vec::new()), b"\xEF\xBB\xBF");
    }
}
