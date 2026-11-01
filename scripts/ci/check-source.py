#!/usr/bin/env python3
"""Portable source checks without rewriting legacy formatting."""
import ast
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]

# A Unix child of the desktop inherits every descriptor not yet close-on-exec
# (tray/src/ChildProcess.h). In tray/src each QProcess therefore goes through
# ChildProcess::inheritStdioOnly(), directly or through a function that passes
# its QProcess parameter there, in the function that creates it (for a member,
# a constructor of its class) and before it starts. Only these files may start
# children otherwise:
CHILD_PROCESS_EXEMPT = {
    "tray/src/ChildProcess.h": "implements inheritStdioOnly() and startDetached()",
    "tray/src/WindowsTerminalBackend.cpp": "the app links it only on Windows, where inheritStdioOnly() does nothing",
}
CHILD_PROCESS_SOURCES = re.compile(r"tray/src/.+\.(?:cpp|h|mm)")
# The desktop parses JSON only through tray/src/JsonParse.h, which reports a
# parse on the GUI thread. There is no GUI-thread exception: owner decision,
# 2026-10-27, the list of files allowed to call JsonParse::local() only shrank
# and is empty at the end of M2 (zerus-604d.11), so local() is gone and any call
# to it is rejected.
JSON_PARSER = "tray/src/JsonParse.cpp"
JSON_DECODER = re.compile(r"\b(?:QJsonDocument\s*::\s*fromJson|QCborValue\s*::\s*fromCbor)\b")
LOCAL_JSON = re.compile(r"\bJsonParse\s*::\s*local\w*\s*\(")
SEE_CHILD_PROCESS = "(tray/src/ChildProcess.h)"
LEXEME = re.compile(r"//|/\*|[\"']")
LITERAL = {'"': re.compile(r'"(?:\\.|[^"\\\n])*"?'), "'": re.compile(r"'(?:\\.|[^'\\\n])*'?")}
# Encoding and raw prefixes; a quote after any other word character separates digits.
LITERAL_PREFIX = re.compile(r"(?:\A|\W)(?:u8|[uUL])?(R?)\Z")
TYPE_HEADER = re.compile(r"\b(?:class|struct|union|enum|namespace|extern)\b(?![^<>]*>)")
BRACKET = re.compile(r"[(){}\[\]]")
ARGUMENT = re.compile(r"[(){}\[\],]")
VARIABLE = r"[A-Za-z_]\w*(?:\s*(?:\.|->)\s*[A-Za-z_]\w*)*"


def code_only(text):
    """The text with comments and literals blanked, keeping every offset and line."""
    out, at = list(text), 0
    while match := LEXEME.search(text, at):
        start = match.start()
        prefix = LITERAL_PREFIX.search(text, max(0, start - 4), start)
        if match[0] == "//":
            end = text.find("\n", start)
        elif match[0] == "/*":
            end = text.find("*/", start + 2) + 2
        elif match[0] == "'" and prefix is None:
            at = start + 1
            continue
        elif prefix and prefix[1]:
            delimiter = text[start + 1:text.find("(", start)]
            end = text.find(")" + delimiter + '"', start)
            end = end + len(delimiter) + 2 if end >= 0 else -1
        else:
            end = LITERAL[match[0]].match(text, start).end()
        end = len(text) if end <= start else end
        out[start:end] = [c if c == "\n" else " " for c in text[start:end]]
        at = end
    return "".join(out)


def closing(code, start):
    """The offset just after the bracket that closes the one at start."""
    depth = 0
    for match in BRACKET.finditer(code, start):
        depth += 1 if match[0] in "({[" else -1
        if depth == 0:
            return match.end()
    return len(code)


def arguments(code, start):
    """(start, end) of each top-level argument of the parenthesis at start."""
    end, depth, spans, last = closing(code, start) - 1, 0, [], start + 1
    for match in ARGUMENT.finditer(code, start + 1, end):
        if match[0] != ",":
            depth += 1 if match[0] in "({[" else -1
        elif depth == 0:
            spans.append((last, match.start()))
            last = match.end()
    return spans + [(last, end)]


def plain(expression):
    """A variable as written, without spaces, a dereference or this->."""
    return re.sub(r"^[*&]*(?:this->)?", "", re.sub(r"\s+", "", expression))


def function_end(code, offset):
    """The end of the outermost function body around offset; None outside functions."""
    stack, header = [], 0
    for match in re.finditer(r"[{};]", code):
        if match[0] == "{":
            text = code[header:match.start()]
            stack.append((match.start(), bool(TYPE_HEADER.search(text)) and ")" not in text))
        elif match[0] == "}" and stack:
            start, type_body = stack.pop()
            if start < offset < match.end() and not type_body and all(outer for _, outer in stack):
                return match.end()
        header = match.end()
    return None


def configurers(code, kind):
    """(function, argument index) of each function here that passes its QProcess parameter to inheritStdioOnly()."""
    found = set()
    for match in re.finditer(rf"\b(?:{kind})\s*[*&]\s*([A-Za-z_]\w*)\s*(?=[,)=])", code):
        depth, opening = 0, match.start() - 1
        while opening >= 0 and code[opening] not in ";{}" and (depth or code[opening] != "("):
            depth += {")": 1, "(": -1}.get(code[opening], 0)
            opening -= 1
        if opening < 0 or code[opening] != "(":
            continue
        function = re.search(r"([A-Za-z_]\w*)\s*\Z", code[max(0, opening - 200):opening])
        body = re.compile(r"(?:\s*(?:const|noexcept|override|final))*\s*\{").match(code, closing(code, opening))
        if function and body and re.search(rf"\binheritStdioOnly\s*\(\s*\*?\s*{match[1]}\s*\)",
                                           code[body.end():closing(code, body.end() - 1)]):
            spans = arguments(code, opening)
            found.add((function[1], next(i for i, (start, end) in enumerate(spans) if start <= match.start() < end)))
    return found


def constructors(code):
    """(start, end) of each constructor body here, out of line (Tool::Tool) or in its class."""
    classes = "|".join(set(re.findall(r"\b(?:class|struct)\s+(?:[A-Z_]+\s+)?([A-Za-z_]\w*)\s*(?:final\s*)?[:{]", code)))
    bodies = []
    for match in re.finditer(rf"\b([A-Za-z_]\w*)\s*::\s*\1\s*\(" + (rf"|(?<![\w:~.>*&])(?:{classes})\s*\(" if classes else ""), code):
        at = closing(code, match.end() - 1)
        # Member initializers such as m_process{new QProcess(this)} may use braces as well.
        while (item := re.compile(r"\s*(?:noexcept\s*)?([:,]\s*[\w:<>]+\s*)?([({])").match(code, at)) and item[1]:
            at = closing(code, item.start(2))
        if item and item[2] == "{":
            bodies.append((item.start(2), closing(code, item.start(2))))
    return bodies


def configuring_constructor(code, names, helpers):
    """(start, end) of the first constructor body here that passes one of the names to inheritStdioOnly()."""
    return next(((first, last) for first, last in constructors(code) if configured(code, names, helpers, first, last)), None)


def creations(code, kind):
    """(offset, end, variable names) of each QProcess made here; names are empty when none receives it."""
    made = [(match.start(), match.end(), [match[1]]) for match in
            re.finditer(rf"(?<![\w:<.])(?<!->)(?:{kind})\s+([A-Za-z_]\w*)\s*(?=[;({{\[])", code)
            if not re.search(r"\btypedef\s*\Z", code[max(0, match.start() - 20):match.start()])]
    # new, make_unique, QSharedPointer::create or a temporary such as QProcess().startDetached(program, arguments).
    for match in re.finditer(rf"\bnew\b\s*(?:\([^()]*\)\s*)?(?:::\s*)?(?:{kind})\b|(?:\bstd\s*::\s*)?\bmake_(?:unique|shared)\s*<\s*(?:{kind})\s*>"
                             rf"|\bQSharedPointer\s*<\s*(?:{kind})\s*>\s*::\s*create\b"
                             rf"|(?<![\w:<.])(?<!->)(?P<temporary>{kind})\s*(?=[({{])", code):
        before = code[max(code.rfind(c, 0, match.start()) for c in ";{}") + 1:match.start()]
        if match["temporary"] and re.search(r"(?:\b(?:public|protected|private|virtual)|[:,])\s*\Z", before):
            continue  # A base class or its constructor, which the subclass rule reports.
        # p = new QProcess, p.reset(new QProcess), unique_ptr<QProcess> p(new ...) or a member initializer.
        if assigned := re.search(rf"(?<!\w)((?:{VARIABLE}\s*=\s*)+)\Z", before):
            names = [plain(part) for part in assigned[1].split("=") if part.strip()]
        elif target := re.search(rf"({VARIABLE})\s*(?:\.|->)\s*reset\s*\(\s*\Z"
                                 rf"|(?:(?<!-)>|(?<!:):|,)\s*([A-Za-z_]\w*)\s*[({{]\s*\Z", before):
            names = [plain(target[1] or target[2])]
        else:
            names = []
        made.append((match.start(), match.end(), names))
    return sorted(made)


def variables(names):
    """A pattern for any of the names, with any spacing around . and ->."""
    return "|".join(r"\s*(?:\.|->)\s*".join(map(re.escape, re.split(r"\.|->", name))) for name in names)


def configured(code, names, helpers, start, end):
    """Offsets in start..end where one of the names goes to inheritStdioOnly(), directly or through a helper."""
    found = [match.start() for match in re.compile(
        rf"\binheritStdioOnly\s*\(\s*\*?\s*(?:this\s*->\s*)?(?:{variables(names)})\s*\)").finditer(code, start, end)]
    for function, position in helpers:
        for match in re.compile(rf"(?<![\w.])(?<!->)\b{function}\s*\(").finditer(code, start, end):
            spans = arguments(code, match.end() - 1)
            if position < len(spans) and plain(code[slice(*spans[position])]) in names:
                found.append(match.start())
    return sorted(found)


def process_aliases(code):
    """Names that using or typedef declares here for QProcess."""
    return [*re.findall(r"\busing\s+(\w+)\s*=\s*(?:::\s*)?QProcess\s*;", code),
            *re.findall(r"\btypedef\s+(?:::\s*)?QProcess\s+(\w+)\s*;", code)]


def child_process_findings(files):
    """"file:line: reason" for each child a tray/src file may start with the GUI's descriptors."""
    checked = {name: text for name, text in files.items()
               if CHILD_PROCESS_SOURCES.fullmatch(name) and name not in CHILD_PROCESS_EXEMPT}
    # An alias a header declares reaches the files that include it, which need not name QProcess.
    shared = sorted({alias for name, text in checked.items() if name.endswith(".h") and "QProcess" in text
                     for alias in process_aliases(code_only(text))})
    mention = re.compile(r"\b(?:" + "|".join(["QProcess", *shared]) + r")\b")
    sources = {name: code_only(text) for name, text in checked.items() if mention.search(text)}
    kinds = {name: "|".join(dict.fromkeys(["QProcess", *shared, *process_aliases(code)])) for name, code in sources.items()}
    helpers = set().union(*(configurers(code, kinds[name]) for name, code in sources.items()))
    problems = []
    for name, code in sources.items():
        kind, found = kinds[name], []
        for match in re.finditer(rf"\b(?:{kind})\s*::\s*(startDetached|startCommand|start|execute)\b", code):
            found.append((match.start(), f"QProcess::{match[1]}() bypasses ChildProcess::inheritStdioOnly(); use "
                                         "ChildProcess::startDetached() or a QProcess passed to inheritStdioOnly()"))
        # A base clause, with or without an access specifier (struct Runner : QProcess).
        for match in re.finditer(rf"\b(?:class|struct)\b[^;{{}}()<>]*:(?!:)[^;{{}}]*?(?<![\w:<])(?:::\s*)?"
                                 rf"(?P<base>{kind})\b(?!\s*::)", code):
            found.append((match.start("base"), "a QProcess subclass starts children this check cannot follow"))
        # The static startDetached(program, ...) and execute() also compile on an object, which they ignore.
        for match in re.finditer(r"(?:\.|->)\s*startDetached\s*\(", code):
            spans = [code[slice(*span)].strip() for span in arguments(code, match.end() - 1)]
            if len(spans) > 1 or spans[0] not in {"", "nullptr"} and not spans[0].startswith("&"):
                found.append((match.start(), "startDetached() with a program is the static QProcess::startDetached(), which "
                                             "bypasses ChildProcess::inheritStdioOnly(); use ChildProcess::startDetached()"))
        for match in re.finditer(rf"(?<![\w:<.])(?<!->)(?:{kind})\s+[A-Za-z_]\w*\s*", code):
            after = closing(code, match.end()) if code[match.end():match.end() + 1] in ("(", "{", "[") else match.end()
            if re.compile(r"\s*,").match(code, after):
                found.append((match.start(), "declare each QProcess on its own so that this check can follow it"))
        made = creations(code, kind)
        for index, (start, end, names) in enumerate(made):
            if not names:
                found.append((start, "assign the new QProcess to a variable and pass it to ChildProcess::inheritStdioOnly()"))
                continue
            scope = function_end(code, start)
            limit = min([scope or len(code)] + [later for later, _, other in made[index + 1:] if set(other) & set(names)])
            if executed := re.compile(rf"(?<![\w.])(?<!->)(?:this\s*->\s*)?(?:{variables(names)})\s*(?:\.|->)\s*execute\s*\(") \
                    .search(code, end, limit):
                found.append((executed.start(), "QProcess::execute() bypasses ChildProcess::inheritStdioOnly(), also on an object; "
                                                "use a QProcess passed to inheritStdioOnly() and waitForFinished()"))
            # The configuration belongs in the rest of the function, or for a member in a constructor of its class,
            # here or in its other sources (Tool.h and Tool.cpp).
            where, text, first = name, code, end
            if scope is None:
                stem, limit = name.rsplit(".", 1)[0], end
                for other in [name, *(other for other in files if other != name and CHILD_PROCESS_SOURCES.fullmatch(other)
                                      and other.rsplit(".", 1)[0] == stem)]:
                    other_code = code if other == name else code_only(files[other])
                    if body := configuring_constructor(other_code, names, helpers):
                        where, text, (first, limit) = other, other_code, body
                        break
            if not (at := configured(text, names, helpers, first, limit)):
                found.append((start, f"QProcess {names[0]} is not passed to ChildProcess::inheritStdioOnly() "
                                     "in the function that creates it"))
            elif started := re.compile(rf"(?<![\w.])(?<!->)(?:this\s*->\s*)?({variables(names)})\s*(\.|->)\s*"
                                       rf"(startDetached|startCommand|start|open)\s*\(").search(text, first, at[0]):
                problems.append((where, text.count("\n", 0, started.start()) + 1,
                                 f"{plain(started[1])}{started[2]}{started[3]}() comes before "
                                 f"ChildProcess::inheritStdioOnly({plain(started[1])})"))
        problems += [(name, code.count("\n", 0, offset) + 1, reason) for offset, reason in found]
    return [f"{name}:{line}: {reason} {SEE_CHILD_PROCESS}." for name, line, reason in sorted(problems)]


def json_parse_findings(files):
    """"file:line: reason" for JSON parsed outside JsonParse.h or on the GUI thread through JsonParse::local()."""
    problems = []
    for name, text in sorted(files.items()):
        code = code_only(text)
        found = [] if name == JSON_PARSER else [
            (match.start(), "parse JSON with JsonParse (tray/src/JsonParse.h), off the GUI thread")
            for match in JSON_DECODER.finditer(code)]
        found += [(match.start(), "JsonParse::local() is gone; parse off the GUI thread (tray/src/JsonParse.h)")
                  for match in LOCAL_JSON.finditer(code)]
        problems += [f"{name}:{code.count(chr(10), 0, offset) + 1}: {reason}" for offset, reason in sorted(found)]
    return problems


def main():
    paths = subprocess.check_output(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], cwd=ROOT).decode().split("\0")
    # First, so that a desktop child the unit tests would also find shows only its findings.
    sources = {name: (ROOT / name).read_text(encoding="utf-8") for name in set(filter(None, paths))
               if CHILD_PROCESS_SOURCES.fullmatch(name) and (ROOT / name).is_file()}
    if stale := sorted(set(CHILD_PROCESS_EXEMPT) - set(sources)):
        raise SystemExit(f"Remove the child process exemption of missing {', '.join(stale)}.")
    if problems := child_process_findings(sources):
        raise SystemExit("\n".join(problems))
    if problems := json_parse_findings(sources):
        raise SystemExit("\n".join(problems))
    subprocess.run([sys.executable, "-m", "unittest", "discover", "-q", "-s", "tests", "-p", "test_source_*.py"], cwd=ROOT, check=True)
    subprocess.run([sys.executable, str(ROOT / "scripts/check-source-privacy.py")], check=True)
    subprocess.run(["git", "diff", "--check"], cwd=ROOT, check=True)
    version = (ROOT / "VERSION").read_text().strip()
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise SystemExit("VERSION must contain one stable semantic version.")
    cmake = (ROOT / "tray/CMakeLists.txt").read_text()
    if f"project(hgs-tray VERSION {version} " not in cmake:
        raise SystemExit("VERSION and the desktop version must match before release.")
    android = (ROOT / "mobile/android/app/build.gradle.kts").read_text()
    mobile_version = re.search(r'val stableVersionName = "([0-9]+\.[0-9]+\.[0-9]+)"', android)
    if not mobile_version:
        raise SystemExit("Cannot verify the stable Android version.")
    for stable, path in ((version, ROOT / "DEV_VERSION"), (mobile_version[1], ROOT / "mobile/android/DEV_VERSION")):
        dev = path.read_text().strip()
        if not re.fullmatch(r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)', dev) or tuple(map(int, dev.split('.'))) <= tuple(map(int, stable.split('.'))):
            raise SystemExit(f"{path.relative_to(ROOT)} must name a release newer than stable.")
    ios_version = (ROOT / "mobile/ios/DEV_VERSION").read_text().strip()
    ios_config = (ROOT / "mobile/ios/Config/Base.xcconfig").read_text()
    if not re.fullmatch(r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)', ios_version) or \
            not re.search(rf'(?m)^MARKETING_VERSION = {re.escape(ios_version)}$', ios_config):
        raise SystemExit("mobile/ios/DEV_VERSION must be X.Y.Z and equal MARKETING_VERSION in Config/Base.xcconfig.")
    ios_stable = ROOT / "mobile/ios/VERSION"
    if ios_stable.exists():
        stable = ios_stable.read_text().strip()
        if not re.fullmatch(r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)', stable) or \
                tuple(map(int, ios_version.split('.'))) <= tuple(map(int, stable.split('.'))):
            raise SystemExit("mobile/ios/DEV_VERSION must name a release newer than mobile/ios/VERSION.")
    # The App Store privacy label is published as linked user content for app
    # functionality, never tracking; the shipped privacy manifest must agree.
    manifest = plistlib.loads((ROOT / "mobile/ios/Zerus/PrivacyInfo.xcprivacy").read_bytes())
    collected = {(item.get("NSPrivacyCollectedDataType"), item.get("NSPrivacyCollectedDataTypeLinked"),
                  item.get("NSPrivacyCollectedDataTypeTracking"), tuple(item.get("NSPrivacyCollectedDataTypePurposes", ())))
                 for item in manifest.get("NSPrivacyCollectedDataTypes", [])}
    expected = {(f"NSPrivacyCollectedDataType{kind}", True, False, ("NSPrivacyCollectedDataTypePurposeAppFunctionality",))
                for kind in ("PhotosorVideos", "AudioData", "OtherUserContent")}
    if manifest.get("NSPrivacyTracking") is not False or collected != expected:
        raise SystemExit("mobile/ios PrivacyInfo.xcprivacy must match the published App Store privacy label.")
    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text())
    if cargo["package"]["license"] != "MIT":
        raise SystemExit("Review the package license and notices.")
    # GitHub enforces these scoped cache permissions. actionlint 1.7.12 does
    # not recognize the new key yet; validate its fixed enum before ignoring
    # only that known syntax warning in actionlint.
    for workflow in (ROOT / ".github/workflows").glob("*.yml"):
        for line in workflow.read_text().splitlines():
            match = re.match(r"^( *)cache-mode:\s*([^#]*)(?:#.*)?$", line)
            if match and (len(match[1]) not in {0, 4} or
                          match[2].strip() not in {"read", "write", "write-only", "none"}):
                raise SystemExit(f"{workflow.name}: cache-mode requires a fixed workflow/job enum.")
    for name in sorted(set(filter(None, paths))):
        path = ROOT / name
        if not path.is_file():
            continue
        if path.suffix == ".py" or name in {"packaging/linux/zerus-setup", "packaging/macos/zerus-setup"}:
            ast.parse(path.read_text(), filename=name)
        elif path.read_bytes().split(b"\n", 1)[0].startswith((b"#!/bin/sh", b"#!/usr/bin/env bash")) or path.name in {"PKGBUILD", "PKGBUILD.in"}:
            # CreateProcess searches System32 before PATH; use the selected Git Bash on Windows.
            subprocess.run([shutil.which("bash") or "bash", "-n", path.as_posix()], check=True)
    print("Source syntax, privacy, child process, JSON parsing and release version checks passed.")


if __name__ == "__main__":
    main()
