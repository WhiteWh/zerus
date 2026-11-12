#!/usr/bin/env python3
"""SDK-free admission/result/project controls; never count these as native UI results."""
import importlib.util
import ast
import copy
import hashlib
import json
import math
import os
from pathlib import Path
import re
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
# Fixture pins include the parent-selection correction; its exact two-term reverse retains full 96fc.


def android_approval_progress():
    """Independent fixed diagnostic wire fixture; never an Android status or final result."""
    setup = [(stage, "-", 0) for stage in ("ON_START", "STORE_BEGIN", "STORE_READY", "BOOTSTRAP_BEGIN",
             "ACTIVITY_LAUNCH_BEGIN", "ACTIVITY_LAUNCH_READY", "BOOTSTRAP_READY")]
    cases = ("countdown_once", "broader_cancel", "broader_confirm", "card_replacement", "feedback_keyboard",
             "lifecycle_no_submit", "readonly_stale_identity")
    steps = setup + [(stage, case, index + (stage == "CASE_COMPLETE")) for index, case in enumerate(cases)
                     for stage in ("CASE_BEGIN", "CASE_COMPLETE")]
    return ''.join(f'INSTRUMENTATION_STATUS: zerus_approval_progress=1|{index}|{stage}|{case}|{complete}|{1000 + index}\n'
                   'INSTRUMENTATION_STATUS_CODE: 1\n' for index, (stage, case, complete) in enumerate(steps, 1))


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


_focus_projection = module("review_focus_projection", "tests/review_focus_projection.py")
original_review_fixture = _focus_projection.original_review_fixture
reverse_focus_wait = _focus_projection.reverse_focus_wait
BASELINES = _focus_projection.BASELINES


def without_native_tool_preparation(source):
    """Reverse only the exact three additive CI setup regions; retain the original UI driver pin."""
    pattern = re.compile(r'^([ ]*)# BEGIN NATIVE_TOOL_PREPARATION ([a-z-]+)\n[\s\S]*?'
                         r'^\1# END NATIVE_TOOL_PREPARATION \2\n', re.M)
    expected = {'environment': '49fab68cc8d71b606afe8b6b7208d32c41c46b385c58fa766251c413054b9356', 'clean-env': 'c58d49ffa7706370446a921cceabdaf83ca674bc3a379fd8fa14ed21e2134c95', 'versions': 'a50cf2b520b2759368ec46bfb151f52dd64e4aaebc17589ab71d56fd68d114c4'}
    matches = list(pattern.finditer(source))
    assert [match[2] for match in matches] == list(expected)
    assert all(hashlib.sha256(match[0].encode()).hexdigest() == expected[match[2]] for match in matches)
    return pattern.sub('', source)


def without_android_focus_diagnostics(source):
    """Remove only the two explicitly additive failure-diagnostic regions for parent equivalence."""
    pattern = re.compile(r'^([ ]*)# BEGIN APPROVAL_FOCUS_DIAGNOSTIC ([a-z-]+)\n[\s\S]*?'
                         r'^\1# END APPROVAL_FOCUS_DIAGNOSTIC \2\n', re.M)
    assert [match[2] for match in pattern.finditer(source)] == ["definition", "capture"]
    return pattern.sub('', source)


def finite_source_predicate(expression, values):
    """Finite source-expression stand-in only; never Kotlin/Swift compilation or native API evidence."""
    tree = ast.parse(expression.strip(), mode="eval")
    allowed = (ast.Expression, ast.BoolOp, ast.And, ast.Or, ast.UnaryOp, ast.Not, ast.Compare,
               ast.Eq, ast.NotEq, ast.Lt, ast.LtE, ast.Gt, ast.GtE, ast.Name, ast.Load, ast.Constant)
    assert all(isinstance(node, allowed) for node in ast.walk(tree))
    assert {node.id for node in ast.walk(tree) if isinstance(node, ast.Name)} <= values.keys()
    return eval(compile(tree, "<finite-source-predicate>", "eval"), {"__builtins__": {}}, values)


def ios_terminal_cleanup_memory(source, mode):
    """Finite source-bound notification/token stand-in, not Swift or native delivery evidence."""
    actions = source.split('    private var actions: [Action] {', 1)[1].split('    private func name(', 1)[0]
    selected = dict(re.findall(r'case \.(\w+): \[([^\]]*)\]', actions))
    terminal = re.search(r'return selected \+ \[\.(\w+)\]', actions)[1]
    assert terminal == 'closeControls'
    names = re.findall(r'\.(\w+)', selected[mode]) + [terminal]
    assert len(names) == len(set(names))
    assert '"test.example.zerus.ui.\\(launch).\\(generation.lowercased()).\\(action.rawValue)"' in source
    consume = source.split('    private func consume(', 1)[1].split('    func close()', 1)[0]
    guard = re.search(r'guard ([^\n]+) else \{', consume)[1]
    predicate = guard.replace('!closed', 'not closed').replace('failure.isEmpty', 'failure_empty').replace(
        'actions.contains(action)', 'mode_admits').replace('!consumed.contains(action)', 'not consumed').replace(
        'tokens[action]', 'owned_token').replace(',', ' and')
    order = ['tokens.removeValue(forKey: action)', 'guard notify_cancel(token) == 0',
             'consumed.insert(action)', 'case .closeControls: close(); return', 'status = "ack|']
    assert [consume.index(term) for term in order] == sorted(consume.index(term) for term in order)
    close = source.split('    func close() {\n', 1)[1].split('\n    }\n}', 1)[0]
    assert close == '''        closed = true
        for action in Array(tokens.keys) {
            guard let token = tokens.removeValue(forKey: action) else { failed("missing-owned-token"); continue }
            if notify_cancel(token) != 0 { failed("close-cancel") }
        }
        if failure.isEmpty { status = "closed" }'''
    state = SimpleNamespace(tokens={name: index + 17 for index, name in enumerate(names)}, consumed=set(),
                            closed=False, failure='', status='ready', cancellations=[], generation='current')

    def failed(reason):
        if not state.failure: state.failure = reason
        state.status = 'error|' + state.failure

    def cancel(token, failures):
        assert token not in state.cancellations, 'No retry of an owned or uncertain cancellation'
        state.cancellations.append(token)
        return token not in failures

    def dispatch(action=terminal, *, generation='current', token=None, failures=()):
        # A wrong generation or unregistered action has no matching notification registration.
        if generation != state.generation or action not in names: return
        received = state.tokens.get(action) if token is None else token
        allowed = finite_source_predicate(predicate, dict(closed=state.closed, failure_empty=not state.failure,
            mode_admits=action in names, consumed=action in state.consumed, owned_token=state.tokens.get(action), token=received))
        if not allowed: failed('stale-or-repeated-action'); return
        owned = state.tokens.pop(action)
        if not cancel(owned, failures): failed('consumption-cancel'); return
        state.consumed.add(action)
        if action == terminal:
            state.closed = True
            for pending in list(state.tokens):
                if not cancel(state.tokens.pop(pending), failures): failed('close-cancel')
            if not state.failure: state.status = 'closed'
        else:
            state.status = 'ack|' + action

    return SimpleNamespace(state=state, dispatch=dispatch,
        qualified=lambda visible_ack=None: state.closed and not state.tokens and not state.failure and visible_ack == 'closed')


def android_diagnostic_memory(source):
    """Source-bound finite callback/line stand-in; no Android, JVM or real window operation."""
    line = source.split('    private fun evidenceLine(', 1)[1].split('    private fun diagnosticStage(', 1)[0]
    limits = re.fullmatch(r'[^\n]+\{\s*if \(lines.size < (\d+)\) lines.add\(text.take\((\d+)\)\)\s*'
                         r'else if \(lines.size == (\d+)\) lines.add\("([^"]+)"\)\s*}\s*', line)
    assert limits and limits[1] == limits[3]
    count, width, overflow = int(limits[1]), int(limits[2]), limits[4]
    assert (count, width, overflow) == (63, 160, "owned-evidence UNKNOWN line-limit")
    stage = source.split('    private fun diagnosticStage(', 1)[1].split('    private fun ownedTreeEvidence(', 1)[0]
    body = stage.split(' {\n', 1)[1].rsplit('    }', 1)[0]
    assert re.fullmatch(r'\s*try \{ inspect\(\) } catch \(error: Throwable\) \{\s*'
                        r'evidenceLine\(lines, "owned-evidence UNKNOWN stage=\$name type=\$\{error.javaClass.simpleName.take\(80\)}"\)\s*}\s*', body)
    # Only the exact source stage's call/catch/append is translated; callbacks replace OS observations.
    translated = 'def stage(lines, name, inspect):\n    try:\n        inspect()\n    except BaseException as error:\n' \
                 '        evidenceLine(lines, "owned-evidence UNKNOWN stage=" + name + " type=" + type(error).__name__[:80])\n'
    def evidence_line(lines, text):
        if len(lines) < count:
            lines.append(text[:width])
        elif len(lines) == count:
            lines.append(overflow)
    namespace = dict(evidenceLine=evidence_line, BaseException=BaseException, type=type, __builtins__={})
    exec(compile(translated, '<android-diagnostic-stage-source-standin>', 'exec'), namespace)
    window = source.split('    private fun windowEvidence()', 1)[1].split('    private fun modelEvidence(', 1)[0]
    stages = re.findall(r'^        diagnosticStage\(lines, "([^"]+)"\)', window, re.M)
    assert stages == ["active-root", "window-list"]
    return evidence_line, namespace["stage"], stages


def cached_send_memory(source, anchor, window, package, end, clock, current_card=lambda: True):
    """Finite source-bound accessibility stand-in, never Android/Kotlin or actual native-node evidence."""
    body = source.split('    private fun cachedSendButton(', 1)[1].split('    private fun tap(', 1)[0]
    for exact in ('val end = checkNotNull(cached.resumeEnd)', 'check(cached.current())',
                  'if (depth > 0 && !current.refresh()) { staleAncestor = true; break }',
                  'current = checkNotNull(current.parent)', 'check(staleAncestor)',
                  'val remaining = end - SystemClock.elapsedRealtime()', 'check(remaining > 0)',
                  'SystemClock.sleep(minOf(30L, remaining))'):
        assert exact in body
    limit = int(re.search(r'for \(depth in 0 until (\d+)\)', body)[1])
    assert limit == 16
    anchor_guard = re.search(r'check\((cached.node.refresh\(\)[\s\S]*?)\) \{\n                "Early-review Send node is stale"', body)[1]
    ancestor_guard = re.search(r'check\((current.windowId[^\n]+)\)', body)[1]
    terminal = re.search(r'if \((current.isClickable[^\n]+)\) \{', body)[1]
    assert re.search(r'if \(current.isClickable \|\| !current.isEnabled\) \{\s*check\(cached.current\(\)\)[^\n]+\s*check\(SystemClock.elapsedRealtime\(\) < end\)[^\n]+\s*return current', body)
    def translated(expression, values):
        for literal, replacement in (
            ('cached.node.refresh()', 'refreshed'), ('cached.window', 'window'), ('cached.node.windowId', 'node_window'),
            ('cached.node.packageName?.toString()', 'node_package'), ('targetContext.packageName', 'package'),
            ('exactVisible(cached.node, "Send answer")', 'exact_visible'), ('current.windowId', 'node_window'),
            ('current.packageName?.toString()', 'node_package'), ('current.isClickable', 'clickable'),
            ('current.isEnabled', 'enabled'), ('&&', ' and '), ('||', ' or '), ('!', 'not ')):
            expression = expression.replace(literal, replacement)
        return finite_source_predicate(' '.join(expression.splitlines()), values)
    walks = sleeps = 0
    if end is None: raise AssertionError('unarmed')
    while True:
        assert clock.now < end
        assert current_card()
        refreshed = anchor.refresh()
        assert translated(anchor_guard, dict(refreshed=refreshed, window=window, node_window=anchor.window,
                          node_package=anchor.package, package=package, exact_visible=anchor.visible and anchor.label == 'Send answer'))
        walks += 1
        node, stale = anchor, False
        for depth in range(limit):
            if depth > 0 and not node.refresh(): stale = True; break
            assert translated(ancestor_guard, dict(node_window=node.window, window=window, node_package=node.package, package=package))
            assert clock.now < end
            if translated(terminal, dict(clickable=node.clickable, enabled=node.enabled)):
                assert current_card()
                assert clock.now < end
                return node, walks, sleeps
            node = node.parent
            assert node is not None
        assert stale
        remaining = end - clock.now
        assert remaining > 0
        clock.now += min(30, remaining)
        sleeps += 1


def prepare_anchor_memory(source, cached, lookup, clock, package):
    """Interpret the finite fixture preparation source over memory fakes; never Kotlin or Android evidence."""
    state = source.split('// BEGIN EARLY_SEND_LIVE_ANCHOR state\n', 1)[1].split('// END EARLY_SEND_LIVE_ANCHOR state', 1)[0]
    setter = re.fullmatch(r'\s*var node: AccessibilityNodeInfo = initialNode\s*private set\s*'
        r'private var anchorPreparationAttempted = false\s*'
        r'fun prepareAnchor\(acquire: \(\) -> AccessibilityNodeInfo\) \{\s*'
        r'check\(!anchorPreparationAttempted\) \{ "([^"]+)" }\s*'
        r'anchorPreparationAttempted = true\s*node = acquire\(\)\s*}\s*', state)
    assert setter and 'private set' in state
    body = source.split('    private fun prepareEarlySendAnchor(cached: EarlySend) {\n', 1)[1].split(
        '    // END EARLY_SEND_LIVE_ANCHOR preparation', 1)[0]
    assert body.startswith('        cached.prepareAnchor {\n') and body.endswith('        }\n    }\n')
    body = body[len('        cached.prepareAnchor {\n'):-len('        }\n    }\n')]
    token = re.compile(r'\s*(?:'
        r'(?P<end>val end = checkNotNull\(cached.resumeEnd\))|'
        r'val fresh = checkNotNull\(visible\("(?P<label>[^"]+)"\)\) \{ "(?P<missing>[^"]+)" }|'
        r'check\((?P<guard>[\s\S]*?)\) \{\s*"(?P<failure>[^"]+)"\s*}|'
        r'(?P<result>fresh)(?=\s*$))')
    operations = []
    while body.strip():
        match = token.match(body)
        assert match, 'unrecognized preparation source'
        operations.append(match.groupdict())
        body = body[match.end():]
    assert [next(key for key in ('end', 'label', 'guard', 'result') if op[key]) for op in operations] == [
        'end', 'guard', 'guard', 'label', 'guard', 'guard', 'guard', 'guard', 'guard', 'guard', 'guard',
        'guard', 'guard', 'result']
    assert [op['label'] for op in operations if op['label']] == ['Send answer']

    def evaluate(expression, values):
        for literal, replacement in (
            ('SystemClock.elapsedRealtime()', 'clock.now'),
            ('fresh.windowId', 'fresh.window'), ('fresh.packageName?.toString()', 'fresh.package'),
            ('targetContext.packageName', 'package'), ('fresh.isVisibleToUser', 'fresh.visible'),
            ('fresh.text?.toString()', 'fresh.text'), ('fresh.contentDescription?.toString()', 'fresh.description'),
            ('&&', ' and '), ('||', ' or ')):
            expression = expression.replace(literal, replacement)
        tree = ast.parse(' '.join(expression.splitlines()), mode='eval')
        allowed = (ast.Expression, ast.BoolOp, ast.And, ast.Or, ast.Compare, ast.Eq, ast.Lt, ast.GtE,
                   ast.Name, ast.Load, ast.Constant, ast.Attribute, ast.Call)
        assert all(isinstance(node, allowed) for node in ast.walk(tree))
        for call in (node for node in ast.walk(tree) if isinstance(node, ast.Call)):
            assert isinstance(call.func, ast.Attribute) and isinstance(call.func.value, ast.Name)
            assert (call.func.value.id, call.func.attr) in {('cached', 'current'), ('fresh', 'refresh')}
            assert not call.args and not call.keywords
        assert {node.id for node in ast.walk(tree) if isinstance(node, ast.Name)} <= values.keys()
        return eval(compile(tree, '<live-anchor-source-predicate>', 'eval'), {'__builtins__': {}}, values)

    assert not cached.attempted, setter[1]
    cached.attempted = True  # The source consumes its sole attempt before the first read, including failed reads.
    values = dict(cached=cached, clock=clock, package=package)
    for operation in operations:
        if operation['end']:
            values['end'] = cached.end
            assert values['end'] is not None
        elif operation['label']:
            values['fresh'] = lookup(operation['label'])
            assert values['fresh'] is not None, operation['missing']
        elif operation['guard']:
            assert evaluate(operation['guard'], values), operation['failure']
        else:
            cached.node = values['fresh']
    return cached.node


def review_clock_memory(source, observation, clock, current_card=lambda: True):
    """Finite source-bound scalar stand-in; no production timer, Kotlin or Android clock is executed."""
    cache = source.split('    private data class EarlySend(', 1)[1].split('    private var earlySend:', 1)[0]
    for exact in ('check(this.resumeStart == null && resumeEnd == null)',
                  'check(resumeStart >= 0 && resumeStart <= SystemClock.elapsedRealtime() && currentCard())',
                  'beforeGeneration = checkNotNull(observation(null)).generation', 'this.resumeStart = resumeStart',
                  'check(reviewStart == null && resumeEnd == null && currentCard())',
                  'val snapshot = observation(checkNotNull(beforeGeneration)) ?: return false',
                  'val start = checkNotNull(detail.reviewStartedAt)',
                  'check(snapshot.generation > checkNotNull(beforeGeneration) && detail.eligible)',
                  'check(checkNotNull(resumeStart) <= start && start <= now)',
                  'val end = Math.addExact(start, 3_000)', 'check(now < end)',
                  'reviewStart = start', 'boundGeneration = snapshot.generation', 'resumeEnd = end',
                  'boundSnapshot = snapshot', 'bound.generation == checkNotNull(boundGeneration) &&',
                  'currentReview(bound, SystemClock.elapsedRealtime(), checkNotNull(resumeEnd))'):
        assert exact in cache
    probe = (ROOT / 'mobile/android/app/src/main/java/app/zerus/mobile/QuestionUiProbe.kt').read_text()
    helper = probe.split('    @Synchronized fun earlyReviewCurrent(', 1)[1].split('    private fun fingerprint(', 1)[0]
    for exact in ('checkNotNull(contentSnapshot(owned, question))',
                  'owned.firstReviewSnapshot(bound.generation - 1) != bound',
                  'bound.record.selection == latest.record.selection && first.eligible && current.eligible &&',
                  'start >= 0 && start <= Long.MAX_VALUE - 3_000 && originalEnd == start + 3_000 &&',
                  'elapsedNow >= start && elapsedNow < originalEnd && currentStart >= start && currentStart <= elapsedNow'):
        assert exact in helper
    state = dict(resume=None, generation=None, start=None, bound=None, end=None)
    def begin(resume):
        assert state['resume'] is None and state['end'] is None
        assert 0 <= resume <= clock.now and current_card()
        state['generation'] = observation(None).generation
        state['resume'] = resume
    def bind():
        assert state['start'] is None and state['end'] is None and current_card()
        record = observation(state['generation'])
        if record is None: return False
        start, now = record.start, clock.now
        assert start is not None
        assert state['generation'] is not None and record.generation > state['generation'] and record.eligible
        assert state['resume'] is not None and state['resume'] <= start <= now
        end = start + 3000
        assert end <= 2**63 - 1 and now < end
        state.update(start=start, bound=record.generation, end=end)
        return True
    def current():
        if not current_card(): return False
        if state['start'] is None: return True
        record = observation(None)
        return state['start'] <= clock.now < state['end'] and record.generation >= state['bound'] and record.eligible and record.start is not None and state['start'] <= record.start <= clock.now
    return SimpleNamespace(begin=begin, bind=bind, current=current, state=state)


def strip_stale_open_phases(source):
    pattern = re.compile(r'^([ ]*)// BEGIN STALE_OPEN_PHASE ([a-z-]+)\n[\s\S]*?^\1// END STALE_OPEN_PHASE \2\n', re.M)
    assert [match[2] for match in pattern.finditer(source)] == ['refresh-main-before', 'refresh-main-returned',
        'title-before', 'title-returned', 'verified-before', 'verified-returned']
    return pattern.sub('', source)


def strip_stale_option_evidence(source):
    source = strip_stale_open_phases(source)
    pattern = re.compile(r'^([ ]*)// BEGIN STALE_OPTION_EVIDENCE ([a-z-]+)\n[\s\S]*?^\1// END STALE_OPTION_EVIDENCE \2\n', re.M)
    assert [match[2] for match in pattern.finditer(source)] == [
        'state', 'failure-stream', 'completed-state', 'delegate-complete', 'return-complete', 'publish']
    return pattern.sub('', source)


def stale_option_evidence_memory(source, facts, *, completed, direct=False, accepted=None, overflow=False, capture_fault=False):
    """Finite source-string/predicate stand-in; no Kotlin, logger, node or instrumentation execution."""
    publish = source.split('// BEGIN STALE_OPTION_EVIDENCE publish\n', 1)[1].split('// END STALE_OPTION_EVIDENCE publish', 1)[0]
    count = int(re.search(r'val rows = facts.take\((\d+)\)', publish)[1])
    width = int(re.search(r'rows.any \{ it.length > (\d+) }', publish)[1])
    cap = int(re.search(r'check\(captured.length <= ([0-9_]+)\)', publish)[1].replace('_', ''))
    assert (count, width, cap) == (48, 512, 25_600)
    assert f'facts.size > {count}' in publish and f'it.take({width})' in publish
    expression = re.search(r'val unknown = ([^\n]+)', publish)[1]
    expression = expression.replace('rows.any { it.length > 512 }', 'over_width').replace('facts.size', 'total').replace(
        'factsOverflow', 'overflow').replace('diagnosticCompleted', 'completed').replace('||', ' or ').replace('!', 'not ')
    branches = publish.split('val outcome = when {\n', 1)[1].split('                        }', 1)[0]
    choices = []
    for line in branches.splitlines():
        match = re.fullmatch(r'\s*(!diagnosticCompleted|directRole|accepted != null|else) -> "([A-Z_]+)"', line)
        assert match
        condition = {'!diagnosticCompleted': 'not completed', 'directRole': 'direct',
                     'accepted != null': 'present', 'else': 'True'}[match[1]]
        choices.append((condition, match[2]))
    assert [value for _, value in choices] == ['UNKNOWN_INTERRUPTED', 'DIRECT_ROLE_DELEGATED', 'OWNER_ACCEPTED', 'NO_OWNER']
    header = re.search(r'val header = "([^"\n]+)"', publish)[1].replace('\\n', '\n')
    marker = re.search(r'val marker = if \(unknown\) "([^"\n]+)" else ""', publish)[1].replace('\\n', '\n')
    fallback = re.search(r'catch \(_: Throwable\) \{\s*staleOptionFailureEvidence = "([^"\n]+)"', publish)[1]
    try:
        if capture_fault: raise MemoryError('controlled diagnostic copy fault')
        rows = facts[:count]
        unknown = finite_source_predicate(expression, dict(over_width=any(len(row) > width for row in rows),
            total=len(facts), overflow=overflow, completed=completed))
        outcome = next(value for condition, value in choices if finite_source_predicate(condition,
            dict(completed=completed, direct=direct, present=accepted is not None)))
        values = {'$status': 'UNKNOWN' if unknown else 'observed', '$outcome': outcome,
            '$diagnosticCompleted': str(completed).lower(), '$directRole': str(direct).lower(),
            '${accepted != null}': str(accepted is not None).lower(), '${rows.size}': str(len(rows))}
        for key, value in values.items(): header = header.replace(key, value)
        assert '$' not in header
        captured = header + '\n'.join(row[:width] for row in rows) + '\n' + (marker if unknown else '')
        assert len(captured) <= cap
        return captured
    except BaseException:
        return fallback


def compose_option_memory(source, label, binding, current=lambda: True):
    """Source-bound predicates over finite memory nodes; never executed Kotlin or native class evidence."""
    body = source.split('            fun composeStaleOptionAction(label: AccessibilityNodeInfo): AccessibilityNodeInfo? {', 1)[1].split(
        '            // END STALE_COMPOSE_ROLE definition', 1)[0]
    ancestor_limit = int(re.search(r'for \(depth in 0 until (\d+)\)', body)[1])
    child_limit = int(re.search(r'check\(children in 1\.\.(\d+)\)', body)[1])
    assert (ancestor_limit, child_limit) == (16, 32)
    for exact in ('check(owner !in seen)', 'check(candidate == null)', 'check(owners.size <= 1)',
                  'check(witnesses.size <= 1)', 'check(owner.childCount == children)',
                  'parent.refresh() && parent == owner',
                  'composeOptionOwner == found.first && composeOptionWitness == found.second &&',
                  'composeOptionBounds == geometry', 'roleOnly(found.second)',
                  'Rect().also(label::getBoundsInScreen) == labelBounds'):
        assert exact in body
    padded = 'if (roleOnly(child) && touchTarget(childBounds, ownerBounds)) witnesses += child to childBounds' in body
    if padded:
        definition = source.split('            // BEGIN STALE_COMPOSE_ROLE definition\n', 1)[1].split(
            '            // END STALE_COMPOSE_ROLE definition', 1)[0]
        for exact in TOUCH_TARGET_SOURCE:
            assert exact in definition
        assert 'Rect().also(found.second::getBoundsInScreen) == witnessBounds[found.second]' in body
    else:
        for exact in ('if (roleOnly(child) && childBounds == ownerBounds)',
                      'Rect().also(found.second::getBoundsInScreen) == found.third'):
            assert exact in body
    def witness_geometry(child, owner):
        return touch_target(child, owner) if padded else child == owner
    owner_expression = body.split('if (owner.className?.toString() == "android.view.View"', 1)[1].split(') {', 1)[0]
    owner_expression = 'owner.className?.toString() == "android.view.View"' + owner_expression
    fresh_expression = body.split('check(found.first.refresh() &&', 1)[1].split(')\n', 1)[0]
    fresh_expression = 'found.first.refresh() &&' + fresh_expression
    role_expression = body.split('fun roleOnly(child: AccessibilityNodeInfo): Boolean =\n', 1)[1].split('                var accepted:', 1)[0].strip()
    click_actions = 'child.actionList.none {\n                        it.id == AccessibilityNodeInfo.ACTION_CLICK || it.id == AccessibilityNodeInfo.ACTION_LONG_CLICK\n                    }'
    assert click_actions in role_expression
    role_expression = role_expression.replace(click_actions, 'no_click_action')
    def evaluate(expression, node, prefix):
        replacements = {'className?.toString()': 'role', 'isVisibleToUser': 'visible', 'isEnabled': 'enabled',
            'isCheckable': 'checkable', 'isChecked': 'checked', 'isSelected': 'selected',
            'isClickable': 'clickable', 'isLongClickable': 'long_clickable', 'childCount': 'child_count',
            'refresh()': 'refreshed', 'windowId': 'window', 'packageName?.toString()': 'package',
            'text.isNullOrEmpty()': 'empty_text', 'contentDescription.isNullOrEmpty()': 'empty_description'}
        for original, name in sorted(replacements.items(), key=lambda item: -len(item[0])):
            expression = expression.replace(prefix + '.' + original, name)
        expression = expression.replace('ownedWindow', 'owned_window').replace('targetContext.packageName', 'owned_package')
        expression = expression.replace('&&', ' and ').replace('||', ' or ').replace('!', 'not ')
        values = {name: getattr(node, name) for name in ('role', 'visible', 'enabled', 'checkable', 'checked',
            'selected', 'clickable', 'long_clickable', 'refreshed', 'window', 'package')}
        values.update(child_count=len(node.children), empty_text=not node.text, empty_description=not node.description,
                      no_click_action=not (node.actions & {'click', 'long-click'}), owned_window=9, owned_package='app.zerus.mobile')
        return finite_source_predicate(' '.join(expression.splitlines()), values)
    def owned(node):
        assert node is not None and node.refreshed and node.window == 9 and node.package == 'app.zerus.mobile'
    def contains(outer, inner):
        return outer[0] <= inner[0] < inner[2] <= outer[2] and outer[1] <= inner[1] < inner[3] <= outer[3]
    assert current()
    owned(label)
    assert label.visible and (label.text == 'Approve once' or label.description == 'Approve once')
    label_bounds = label.bounds
    assert contains(label_bounds, label_bounds)
    node, seen, owners = label, [], []
    for _ in range(ancestor_limit):
        if node is None: break
        assert all(node is not old for old in seen)
        seen.append(node); owned(node)
        if node.role == 'android.widget.RadioButton':
            assert not binding
            return None  # Delegate to the original direct-role walker, rather than accepting it here.
        if evaluate(owner_expression, node, 'owner'):
            assert contains((0, 0, 1080, 2400), node.bounds) and contains(node.bounds, label_bounds)
            children = tuple(node.children)
            assert 1 <= len(children) <= child_limit
            witnesses = []
            for child in children:
                owned(child); owned(child.parent)
                assert child.parent is node
                if child.role == 'android.widget.RadioButton' and evaluate(role_expression, child, 'child') and \
                        witness_geometry(child.bounds, node.bounds):
                    witnesses.append(child)
            assert tuple(node.children) == children and len(witnesses) <= 1
            if witnesses: owners.append((node, witnesses[0], node.bounds, witnesses[0].bounds))
        node = node.parent
    assert node is None and len(owners) <= 1 and current()
    if not owners: return None
    owner, witness, bounds, witness_bounds = owners[0]
    owned(owner); owned(witness); owned(label)
    assert evaluate(fresh_expression, owner, 'found.first') and evaluate(role_expression, witness, 'child')
    assert witness.parent is owner and owner.bounds == bounds and witness.bounds == witness_bounds and label.bounds == label_bounds
    found = (owner, witness, bounds, label_bounds)
    if binding:
        assert binding['owner'] is owner and binding['witness'] is witness and binding['geometry'] == found[2:]
    else:
        binding.update(owner=owner, witness=witness, geometry=found[2:])
    return owner


TOUCH_TARGET_SOURCE = (
    'val minimum = 48f * targetContext.resources.displayMetrics.density',
    'if (inner == outer) return innerStart == outerStart',
    'return inner < outer && kotlin.math.abs(outer - minimum) <= 1f &&',
    'kotlin.math.abs((innerStart - outerStart) - (outerEnd - innerEnd)) <= 1',
    'return !child.isEmpty && owner.contains(child) &&',
)

SEND_ROLE_SOURCE = (
    'if (depth == 0 && text == "Send answer") composeStaleButtonAction(label)?.let { return it }',
    'child.className?.toString() == "android.widget.Button" && child.isVisibleToUser &&',
    'if (owner.className?.toString() == "android.widget.Button") return null',
    'if (owner.className?.toString() == "android.view.View" && owner.isVisibleToUser &&\n                            !owner.isEnabled && owner.isClickable) {',
    'if (roleOnlyButton(child) && touchTarget(Rect().also(child::getBoundsInScreen), ownerBounds)) witnesses += child',
    'check(witnesses.size <= 1) { "Known-stale Send role witness is ambiguous" }',
    'check(owners.size <= 1) { "Known-stale Send owner is ambiguous" }',
    'found.first.actionList.none { it.id == AccessibilityNodeInfo.ACTION_CLICK } &&',
    'check(found.second.refresh() && found.second.parent == found.first && roleOnlyButton(found.second))',
)


def touch_target(child, owner, density=2.625):
    """Finite stand-in for touchTarget on the pinned 1080x2400 420 dpi emulator, not Compose execution."""
    minimum = 48 * density
    def axis(inner_start, inner_end, outer_start, outer_end):
        inner, outer = inner_end - inner_start, outer_end - outer_start
        if inner == outer: return inner_start == outer_start
        return inner < outer and abs(outer - minimum) <= 1 and abs((inner_start - outer_start) - (outer_end - inner_end)) <= 1
    empty = child[0] >= child[2] or child[1] >= child[3]
    inside = owner[0] <= child[0] and owner[1] <= child[1] and child[2] <= owner[2] and child[3] <= owner[3]
    return not empty and inside and axis(child[0], child[2], owner[0], owner[2]) and axis(child[1], child[3], owner[1], owner[3])


def restore_compose_role_children(source):
    """Reverse only the touch-target and Send role-child admissions to the exact 304b bytes; no Kotlin execution."""
    helper = source.split('            // Compose reports the owner with its 48 dp minimum touch target; the role\n', 1)
    assert len(helper) == 2
    tail = helper[1].split('            // Compose UI 1.8.3 emits a role-only child for a merging selectable owner with children.\n', 1)
    assert len(tail) == 2 and 'fun composeStaleButtonAction(label: AccessibilityNodeInfo)' in tail[0]
    source = helper[0] + '            // Compose UI 1.8.3 emits a role-only child for a merging selectable owner with children.\n' + tail[1]
    terms = (
        ('                    if (depth == 0 && text == "Send answer") composeStaleButtonAction(label)?.let { return it }\n', ''),
        ('                    val witnessBounds = mutableMapOf<AccessibilityNodeInfo, Rect>()\n', ''),
        ('val witnesses = mutableListOf<Pair<AccessibilityNodeInfo, Rect>>()', 'val witnesses = mutableListOf<AccessibilityNodeInfo>()'),
        ('if (roleOnly(child) && touchTarget(childBounds, ownerBounds)) witnesses += child to childBounds',
         'if (roleOnly(child) && childBounds == ownerBounds) witnesses += child'),
        ('''                            if (witnesses.size == 1) {
                                owners += Triple(owner, witnesses.single().first, Rect(ownerBounds))
                                witnessBounds[witnesses.single().first] = witnesses.single().second
                            }
''', '                            if (witnesses.size == 1) owners += Triple(owner, witnesses.single(), Rect(ownerBounds))\n'),
        ('Rect().also(found.second::getBoundsInScreen) == witnessBounds[found.second]',
         'Rect().also(found.second::getBoundsInScreen) == found.third'),
    )
    for corrected, original in terms:
        assert source.count(corrected) == 1
        source = source.replace(corrected, original, 1)
    return source


def restore_stale_owner_checked(source):
    """Reverse only the two reviewed parent-selection preconditions; no Kotlin execution."""
    terms = (
        ('!owner.isEnabled && owner.isCheckable) {', '!owner.isEnabled && owner.isCheckable && owner.isChecked) {'),
        ('found.first.isCheckable &&\n', 'found.first.isCheckable && found.first.isChecked &&\n'),
    )
    for corrected, original in terms:
        assert source.count(corrected) == 1 and original not in source
        source = source.replace(corrected, original, 1)
    return source


def compose_option_nodes():
    """Finite synthetic role-child shape from pinned source, not an observed Android hierarchy."""
    def node(**changes):
        values = dict(role='android.view.View', visible=True, enabled=True, checkable=False, checked=False,
            selected=False, clickable=False, long_clickable=False, text='', description='', actions=set(),
            children=[], parent=None, refreshed=True, window=9, package='app.zerus.mobile', bounds=(0, 0, 1000, 2000))
        values.update(changes)
        return SimpleNamespace(**values)
    root = node()
    owner = node(enabled=False, checkable=True, checked=True, parent=root, bounds=(10, 100, 990, 300))
    label = node(role='android.widget.TextView', text='Approve once', parent=owner, bounds=(80, 140, 400, 200))
    witness = node(role='android.widget.RadioButton', parent=owner, bounds=owner.bounds)
    owner.children = [label, witness]; root.children = [owner]
    return SimpleNamespace(root=root, owner=owner, label=label, witness=witness, node=node)


def early_probe_main_depths(source):
    """Bounded source call graph of the four fixed local callables; never Android/main-thread execution."""
    capture = source.split('        fun captureEarlySend(): EarlySend {', 1)[1].split('        fun earlyReview(', 1)[0]
    predicate = re.search(r'            fun currentCard\(\): Boolean \{([\s\S]*?)\n            }', capture)[1]
    # The only callable in this scalar on-main predicate is the original bounded model-list observation.
    assert re.findall(r'\b(\w+)\s*\(', predicate) == ['singleOrNull']
    assert 'return model.selected?.target == target && model.activityVerified && card != null && card.id == id && card.hash == hash &&' in predicate
    assert 'card.run == target.run && card.conversation == target.conversation' in predicate
    current = capture.split('            val current = ', 1)[1].split('            check(earlySend == null', 1)[0].strip()
    observation = capture.split('            val observation = ', 1)[1].split('            val currentReview = ', 1)[0].strip()
    current_review = capture.split('            val currentReview = ', 1)[1].split('            return EarlySend(', 1)[0].strip()
    current = re.fullmatch(r'\{\s*main\s*\{([\s\S]*)}\s*}', current)[1]
    observation = re.fullmatch(r'\{\s*afterGeneration: Int\?\s*->\s*main\s*\{([\s\S]*)}\s*}', observation)[1]
    current_review = re.fullmatch(r'\{\s*bound: QuestionUiProbe.ContentSnapshot, now: Long, end: Long\s*->\s*main\s*\{([\s\S]*)}\s*}', current_review)[1]
    assert re.fullmatch(r'\s*currentCard\(\)\s*', current)
    bodies = dict(currentCard=predicate, current=current, observation=observation, currentReview=current_review)
    roots = dict(currentCard=0, current=1, observation=1, currentReview=1)
    def depth(name, path=()):
        assert name not in path and len(path) < 3
        assert not re.search(r'\b(main|runOnMainSync)\s*(?:\(|\{)', bodies[name])
        calls = re.findall(r'\b(currentCard|current|observation|currentReview)\s*\(', bodies[name])
        return roots[name] + max([depth(call, path + (name,)) for call in calls] or [0])
    result = {name: depth(name) for name in bodies}
    assert result == dict(currentCard=0, current=1, observation=1, currentReview=1)
    return result


def kotlin_brace_scopes(source, offset):
    """Brace scopes for the fixed source layout guard; skip comments and quoted text, not a Kotlin compiler."""
    lexemes = re.compile(r'//[^\n]*|/\*[\s\S]*?\*/|"""[\s\S]*?"""|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'|[{}]')
    stack = []
    for match in lexemes.finditer(source, 0, offset):
        if match[0] == '{':
            stack.append(match.start())
        elif match[0] == '}':
            if not stack:
                raise ValueError('unbalanced-layout-source')
            stack.pop()
    return tuple(stack)


def question_scroll_hint_findings(source):
    """The hint must reserve an ordinary Column sibling, outside the weighted disclosure and beside the footer."""
    hint = 'if (bodyScroll.canScrollForward) Surface('
    column = 'Column(Modifier.then(if(inDialog) Modifier.fillMaxSize() else Modifier.heightIn(max = contentLimit))'
    box = 'Box(Modifier.fillMaxWidth().weight(1f, fill = inDialog).then(disclosureVisibility))'
    footer = 'otherPrompts.forEachIndexed { index,(prompt,field) ->'
    if any(source.count(marker) != 1 for marker in (hint, column, box, footer)):
        return ['missing-or-ambiguous-question-layout']
    hint_at, column_at, box_at, footer_at = (source.index(marker) for marker in (hint, column, box, footer))
    hint_scope = kotlin_brace_scopes(source, hint_at)
    column_open = source.index('{', column_at)
    box_open = source.index('{', box_at)
    findings = []
    if not hint_scope or hint_scope[-1] != column_open:
        findings.append('hint-must-reserve-direct-column-space')
    if box_open in hint_scope:
        findings.append('hint-overlaps-weighted-disclosure')
    if hint_scope != kotlin_brace_scopes(source, footer_at):
        findings.append('hint-and-footer-must-be-siblings')
    header = source[hint_at:source.index('{', hint_at)]
    if 'Modifier.align(Alignment.CenterHorizontally)' not in header:
        findings.append('hint-must-use-column-alignment')
    if any(modifier in header for modifier in ('.offset(', '.zIndex(', '.height(', '.size(', '.weight(', '.graphicsLayer(')):
        findings.append('hint-must-retain-measured-sibling-size')
    return findings


def ios_cleanup_memory(*, now=100.0, whole=900.0, duration=0.0, spawn_delay=0.0, live_after_wait=False):
    """Execute the exact nested driver functions with memory IO/process/clock only."""
    source = (ROOT / "scripts/ci/ios-approval-ui.py").read_text()
    main = next(node for node in ast.parse(source).body if isinstance(node, ast.FunctionDef) and node.name == "main")
    selected = [copy.deepcopy(node) for node in main.body if isinstance(node, ast.FunctionDef)
                and node.name in {"save", "enter_cleanup", "run"}]
    assert {node.name for node in selected} == {"save", "enter_cleanup", "run"}
    tail = next(node for node in main.body if isinstance(node, ast.Try)).finalbody[-2:]
    assert isinstance(tail[0], ast.Assign) and tail[0].targets[0].id == "success"
    assert isinstance(tail[1], ast.Expr) and tail[1].value.func.id == "save"
    final = main.body[-1]
    assert isinstance(final, ast.Assert) and "final persistence" in ast.literal_eval(final.msg)
    clock = SimpleNamespace(now=now)
    events, files, descriptors, children, faults = [], {}, {}, [], {}

    class MemoryFile:
        def __init__(self, name):
            self.name = name; self.data = []; self.fd = len(descriptors) + 10
            descriptors[self.fd] = self
        def __enter__(self): return self
        def __exit__(self, *error):
            events.append(("close", self.name))
            if faults.get("close_name") == self.name:
                clock.now += faults.pop("close_delay")
        def write(self, value): self.data.append(value); return len(value)
        def flush(self): pass
        def fileno(self): return self.fd

    class MemoryPath:
        def __init__(self, name): self.name = name
        def __truediv__(self, child): return MemoryPath(self.name + "/" + child)
        def open(self, mode):
            assert mode in {"x", "xb"} and self.name not in files
            value = MemoryFile(self.name); files[self.name] = value
            return value

    class Pipe:
        def __init__(self, identifier): self.identifier = identifier; self.reads = 0
        def fileno(self): return self.identifier
        def close(self): events.append(("pipe-close", self.identifier))

    class Child:
        pid = 41
        def __init__(self, argv, **options):
            assert options["stdin"] == "memory-DEVNULL" and options["stdout"] == options["stderr"] == "memory-PIPE"
            self.stdout, self.stderr = Pipe(1), Pipe(2)
            self.returncode = None; self.signaled = False
            children.append(self); events.append(("spawn", argv)); clock.now += spawn_delay
            self.ready_at = clock.now + duration
        def wait(self, timeout):
            assert timeout > 0; events.append(("wait", timeout))
            if faults.get("fail_reap") or (live_after_wait and not self.signaled):
                clock.now += timeout
                raise TimeoutError("memory process stayed live")
            self.returncode = 0
        def poll(self): return self.returncode
        def terminate(self): self.signaled = True; events.append(("terminate", self.pid))
        def kill(self): self.signaled = True; events.append(("kill", self.pid))

    class Selector:
        def __init__(self): self.keys = {}
        def __enter__(self): return self
        def __exit__(self, *error): pass
        def register(self, pipe, event, output): self.keys[pipe] = SimpleNamespace(fileobj=pipe, data=output)
        def unregister(self, pipe): del self.keys[pipe]
        def get_map(self): return self.keys
        def select(self, timeout):
            assert 0 <= timeout <= .1
            child = children[-1]
            if clock.now < child.ready_at:
                clock.now = min(child.ready_at, clock.now + timeout)
                if clock.now < child.ready_at: return []
            return [(key, 1) for key in self.keys.values()]

    payload = json.dumps({"devices": {"runtime": [{"udid": "owned", "name": "fixture", "state": "Shutdown"}]}}).encode()
    def read(identifier, amount):
        assert amount == 65536
        pipe = children[-1].stdout if identifier == 1 else children[-1].stderr
        pipe.reads += 1
        return payload if identifier == 1 and pipe.reads == 1 else b""
    def fsync(identifier):
        name = descriptors[identifier].name; events.append(("fsync", name))
        if faults.get("fsync_name") == name:
            clock.now += faults.pop("fsync_delay")

    class MemoryBuffer(bytearray):
        def decode(self, *arguments, **keywords):
            value = super().decode(*arguments, **keywords)
            clock.now += faults.pop("decode_delay", 0)
            return value

    namespace = dict(whole=whole, root=MemoryPath("/evidence"), project="/project", environment={},
                     time=SimpleNamespace(monotonic=lambda: clock.now), json=json, bytearray=MemoryBuffer,
                     os=SimpleNamespace(fsync=fsync, read=read),
                     subprocess=SimpleNamespace(Popen=Child, DEVNULL="memory-DEVNULL", PIPE="memory-PIPE"),
                     selectors=SimpleNamespace(DefaultSelector=Selector, EVENT_READ=1))
    wrapper = ast.parse("def bind():\n    end = whole\n    cleanup_started = cleanup_end = None\n    unknown_child = False\n    commands = []\n").body[0]
    wrapper.body.extend(selected)
    wrapper.body.extend(ast.parse("def state():\n    return dict(end=end, cleanup_started=cleanup_started, cleanup_end=cleanup_end, unknown_child=unknown_child, commands=commands)\n").body)
    wrapper.body.extend(ast.parse("return enter_cleanup, run, save, state").body)
    exec(compile(ast.fix_missing_locations(ast.Module(body=[wrapper], type_ignores=[])), "<ios-cleanup-memory-source>", "exec"), namespace)
    enter, run, save, state = namespace["bind"]()
    final_code = compile(ast.Module(body=copy.deepcopy([*tail, final]), type_ignores=[]), "<ios-cleanup-memory-final>", "exec")
    def finalize():
        values = {**namespace, **state(), "save": save, "success": True, "same": True, "tools_same": True, "cleanup": []}
        exec(final_code, values)
        return values
    return SimpleNamespace(clock=clock, enter=enter, run=run, save=save, state=state, finalize=finalize,
                           faults=faults, events=events, files=files, children=children)


class IosCleanupBudgetTests(unittest.TestCase):
    def test_cleanup_end_is_once_and_clamped_by_original_whole(self):
        for whole, expected in ((900, 160), (125, 125)):
            memory = ios_cleanup_memory(whole=whole)
            self.assertEqual(memory.enter(), expected)
            memory.clock.now += 30
            self.assertEqual(memory.enter(), expected)
            self.assertEqual(memory.state()["cleanup_started"], 100)

    def test_exact_identity_metadata_can_finish_after_eight_seconds_within_fifteen(self):
        memory = ios_cleanup_memory(duration=8.043)
        value = json.loads(memory.run("cleanup-identity", ["memory-simctl", "list", "devices"], 15, True))
        ios = module("ios_approval_ui", "scripts/ci/ios-approval-ui.py")
        self.assertEqual(ios.owned_device(value, "runtime", "owned", "fixture")["state"], "Shutdown")
        self.assertAlmostEqual(memory.clock.now, 108.043)
        self.assertEqual(memory.state()["commands"][0]["deadline_monotonic"], 115)
        self.assertFalse(memory.state()["unknown_child"])
        self.assertTrue(memory.finalize()["success"])

    def test_metadata_cap_expiry_refuses_and_preserves_owned_stop_uncertainty(self):
        memory = ios_cleanup_memory(duration=15)
        with self.assertRaisesRegex(AssertionError, "command deadline"):
            memory.run("cleanup-identity", ["memory-simctl"], 15, True)
        self.assertEqual(len(memory.children), 1)
        self.assertTrue(memory.state()["unknown_child"])
        self.assertIn(("terminate", 41), memory.events)
        with self.assertRaises(AssertionError): memory.finalize()

    def test_expired_sixty_or_whole_end_never_admits_a_following_command(self):
        for whole, expired in ((900, 160), (125, 125)):
            memory = ios_cleanup_memory(whole=whole)
            memory.run("cleanup-identity", ["memory-simctl"], 15, True)
            original_files = set(memory.files)
            memory.clock.now = expired
            with self.assertRaises(AssertionError): memory.run("cleanup-stopped", ["memory-simctl"], 15, True)
            self.assertEqual(len(memory.children), 1)
            self.assertEqual(set(memory.files), original_files)
            with self.assertRaises(AssertionError): memory.finalize()

    def test_earlier_owned_stop_starts_the_same_phase_and_reap_stays_inside_it(self):
        memory = ios_cleanup_memory(duration=1.1)
        with self.assertRaises(AssertionError): memory.run("work", ["memory-child"], 1)
        first_end = memory.state()["cleanup_end"]
        self.assertAlmostEqual(first_end, 161)
        memory.clock.now = 150
        self.assertEqual(memory.enter(), first_end)
        self.assertTrue(memory.state()["commands"][0]["reaped"])
        self.assertTrue(all(0 < event[1] <= 3 for event in memory.events if event[0] == "wait"))
        memory.clock.now = first_end - .5
        with self.assertRaises(AssertionError): memory.run("cleanup-stopped", ["memory-simctl"], 15, True)
        self.assertEqual(memory.state()["cleanup_end"], first_end)
        self.assertEqual([event for event in memory.events if event[0] == "terminate"], [("terminate", 41)])

    def test_unreaped_owned_child_stays_unknown_and_cannot_qualify(self):
        memory = ios_cleanup_memory(duration=1.1)
        memory.faults["fail_reap"] = True
        with self.assertRaises(AssertionError): memory.run("work", ["memory-child"], 1)
        command = memory.state()["commands"][0]
        self.assertFalse(command["reaped"])
        self.assertFalse(command["forced_child_stop"])
        self.assertTrue(memory.state()["unknown_child"])
        self.assertAlmostEqual(memory.state()["cleanup_end"], 161)
        self.assertEqual([event for event in memory.events if event[0] == "wait"], [("wait", 3), ("wait", 3)])
        with self.assertRaises(AssertionError): memory.finalize()

    def test_command_cap_is_not_renewed_after_admission_or_spawn(self):
        memory = ios_cleanup_memory(spawn_delay=15)
        with self.assertRaises(AssertionError): memory.run("cleanup-identity", ["memory-simctl"], 15, True)
        self.assertEqual(memory.state()["commands"][0]["deadline_monotonic"], 115)
        memory = ios_cleanup_memory()
        memory.faults.update(fsync_name="/evidence/cleanup-identity.command.json", fsync_delay=15)
        with self.assertRaisesRegex(AssertionError, "before spawn"):
            memory.run("cleanup-identity", ["memory-simctl"], 15, True)
        self.assertEqual(memory.children, [])

    def test_late_fsync_and_final_file_close_cannot_qualify(self):
        memory = ios_cleanup_memory()
        memory.faults["decode_delay"] = 15
        with self.assertRaisesRegex(AssertionError, "after persistence and close"):
            memory.run("cleanup-identity", ["memory-simctl"], 15, True)
        for stage in ("fsync", "close"):
            memory = ios_cleanup_memory()
            memory.faults.update({stage + "_name": "/evidence/cleanup-identity.stdout", stage + "_delay": 15})
            with self.subTest(stage=stage), self.assertRaisesRegex(AssertionError, "after persistence and close"):
                memory.run("cleanup-identity", ["memory-simctl"], 15, True)
            memory = ios_cleanup_memory()
            memory.enter()
            memory.faults.update({stage + "_name": "/evidence/outcome.json", stage + "_delay": 60})
            with self.subTest(final_stage=stage), self.assertRaisesRegex(AssertionError, "final persistence"):
                memory.finalize()


class IosTerminalCleanupTests(unittest.TestCase):
    def setUp(self):
        self.host = (ROOT / 'mobile/ios/ApprovalUIFixture/ApprovalUIFixtureApp.swift').read_text()
        self.tests = (ROOT / 'mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift').read_text()

    def test_exact_reverse_retains_all_seven_cases_and_every_other_fixture_statement(self):
        host = self.host
        replacements = [
            ('/// Four fixed review controls and one terminal cleanup notification in this disposable fixture only.',
             '/// Four fixed, payload-free notifications inside this disposable simulator fixture only.'),
            ('        case closeControls = "close-controls"\n', ''),
            ('            switch self { case .beginCountdownReview: "c"; case .beginFeedbackReview: "f"; case .replace, .becomeUnavailable: nil\n'
             '            // Cleanup is acknowledged by the actual visible closed status after all cancellations.\n'
             '            case .closeControls: nil }',
             '            switch self { case .beginCountdownReview: "c"; case .beginFeedbackReview: "f"; case .replace, .becomeUnavailable: nil }'),
            ('        let selected: [Action] = switch mode {', '        switch mode {'),
            ('        return selected + [.closeControls]\n', ''),
            ('        switch action { case .closeControls: close(); return\n        case .beginFeedbackReview,',
             '        switch action { case .beginFeedbackReview,'),
        ]
        for new, old in replacements:
            self.assertEqual(host.count(new), 1)
            host = host.replace(new, old, 1)
        self.assertEqual(hashlib.sha256(host.encode()).hexdigest(),
                         '7ae7121e881888143e6ad57846f7ebef8d18d70a4e5f0acfc66ee85180339ab7')
        source = self.tests
        for new, old in [
            ('"become-unavailable", "close-controls"].contains(action)', '"become-unavailable"].contains(action)'),
            ('        // Teardown owns registrations, not native input delivery. A missed UI tap cannot strand them.\n'
             '        // This terminal event has the same launch/generation and is sent once; posting is not its ACK.\n'
             '        post("close-controls")', '        app.buttons["Close controls"].tap()'),
        ]:
            self.assertEqual(source.count(new), 1)
            source = source.replace(new, old, 1)
        self.assertEqual(hashlib.sha256(source.encode()).hexdigest(),
                         'c78cadac19a029a021c9f03ca292ffdcb669250369383100efa6458ce7b5aa4b')

    def test_terminal_dispatch_is_owned_in_every_mode_and_closes_each_token_once(self):
        for mode in ('normal', 'feedback', 'replacement', 'countdown', 'unavailable'):
            with self.subTest(mode=mode):
                fixture = ios_terminal_cleanup_memory(self.host, mode)
                tokens = dict(fixture.state.tokens)
                fixture.dispatch()
                self.assertEqual(fixture.state.cancellations[0], tokens['closeControls'])
                self.assertCountEqual(fixture.state.cancellations, tokens.values())
                self.assertEqual(fixture.state.status, 'closed')
                self.assertFalse(fixture.qualified())
                self.assertTrue(fixture.qualified('closed'))
        self.assertIn('Button("Close controls") { app.controls.close() }', self.host)
        name = 'test.example.zerus.ui.' + 'a' * 36 + '.' + 'b' * 36 + '.close-controls'
        self.assertLessEqual(len(name.encode()), 128)

    def test_stale_generation_action_token_repeat_and_unknown_cancellation_never_qualify(self):
        for values in (dict(generation='stale'), dict(action='unknown'), dict(action='replace')):
            fixture = ios_terminal_cleanup_memory(self.host, 'feedback')
            tokens = dict(fixture.state.tokens)
            fixture.dispatch(**values)
            self.assertEqual(fixture.state.tokens, tokens)
            self.assertFalse(fixture.qualified('closed'))
        fixture = ios_terminal_cleanup_memory(self.host, 'feedback')
        fixture.dispatch(token=999)
        self.assertEqual(fixture.state.status, 'error|stale-or-repeated-action')
        self.assertEqual(fixture.state.cancellations, [])
        self.assertFalse(fixture.qualified('closed'))
        fixture = ios_terminal_cleanup_memory(self.host, 'feedback')
        fixture.dispatch(); cancelled = list(fixture.state.cancellations)
        fixture.dispatch(token=cancelled[0])
        self.assertEqual(fixture.state.cancellations, cancelled)
        self.assertFalse(fixture.qualified('closed'))
        for failed_action in ('closeControls', 'beginFeedbackReview'):
            fixture = ios_terminal_cleanup_memory(self.host, 'feedback')
            token = fixture.state.tokens[failed_action]
            fixture.dispatch(failures=(token,))
            cancelled = list(fixture.state.cancellations)
            fixture.dispatch(token=token)
            self.assertEqual(fixture.state.cancellations, cancelled)
            self.assertEqual(cancelled.count(token), 1)
            self.assertFalse(fixture.qualified('closed'))

    def test_missing_visible_ack_cannot_be_replaced_by_post_success_or_an_unrelated_status(self):
        cleanup = self.tests.split('    private func closeFixture() {', 1)[1].split('    private var send:', 1)[0]
        self.assertEqual(cleanup.count('post("close-controls")'), 1)
        self.assertNotIn('app.buttons["Close controls"].tap()', cleanup)
        self.assertIn('defer { app?.terminate() }', cleanup)
        self.assertIn('NSPredicate(format: "label == \'closed\'")', cleanup)
        self.assertIn('XCTWaiter.wait(for: [closed], timeout: 2), .completed', cleanup)
        self.assertLess(cleanup.index('post("close-controls")'), cleanup.index('XCTWaiter.wait(for: [closed]'))
        self.assertNotIn('sleep', cleanup)
        # Counterexample: successful post with no delivered callback cannot acknowledge teardown.
        fixture = ios_terminal_cleanup_memory(self.host, 'feedback')
        self.assertFalse(fixture.qualified('closed'))
        fixture.dispatch()
        for visible in (None, 'ready', 'ack|begin-feedback-review', 'error|close-cancel'):
            self.assertFalse(fixture.qualified(visible))
        self.assertTrue(fixture.qualified('closed'))


class ApprovalHarnessTests(unittest.TestCase):
    def setUp(self):
        self.ui = module("android_approval_ui", "scripts/ci/android-project-ui.py")

    def output(self, ids=None, count="7", code="-1"):
        return (android_approval_progress() + f"INSTRUMENTATION_RESULT: approval_cases={count}\n"
                f"INSTRUMENTATION_RESULT: approval_case_ids={ids or self.ui.CASES}\n"
                "INSTRUMENTATION_RESULT: stream=Approval choices smoke passed: synthetic only\n\n"
                f"INSTRUMENTATION_CODE: {code}\n")

    def test_original_project_suite_is_the_default(self):
        self.ui.select_suite([])
        self.assertEqual(self.ui.RUNNER, "app.zerus.mobile.SessionProjectSmokeInstrumentation")
        self.assertEqual(self.ui.RESULT_NAMESPACE, "project")
        self.assertEqual(len(self.ui.CASES.split(",")), 6)

    def test_approval_requires_all_seven_exact_cases_in_order(self):
        self.ui.select_suite(["--approval-choices"])
        self.assertEqual(self.ui.instrumentation_result(self.output())["approval_cases"], "7")
        for value in (self.output(count="6"), self.output(count="07"), self.output(ids=",".join(reversed(self.ui.CASES.split(",")))),
                      self.output(ids=self.ui.CASES + ",extra"), self.output(code="0"), self.output().replace("passed:", "failed:")):
            with self.subTest(value=value), self.assertRaises(AssertionError):
                self.ui.instrumentation_result(value)

    def test_project_result_cannot_qualify_an_approval_campaign(self):
        self.ui.select_suite(["--approval-choices"])
        with self.assertRaises(AssertionError):
            self.ui.instrumentation_result(self.output().replace("approval_cases", "project_cases"))
        self.assertEqual(self.ui.RUNNER, "app.zerus.mobile.ApprovalChoicesSmokeInstrumentation")
        self.assertIn("Lapp/zerus/mobile/QuestionUiKt;", self.ui.APP_CLASSES)

    def test_android_progress_never_qualifies_without_every_original_final_oracle(self):
        self.ui.select_suite(["--approval-choices"])
        progress = android_approval_progress()
        final = self.output().removeprefix(progress)
        self.assertEqual(self.ui.instrumentation_result(progress + final)["approval_cases"], "7")
        for prefix in ('', progress, progress + final.split('INSTRUMENTATION_CODE:')[0]):
            with self.subTest(prefix_length=len(prefix)), self.assertRaises(AssertionError):
                self.ui.instrumentation_result(prefix)
        lines = progress.splitlines(keepends=True)
        self.assertEqual(len(lines), 42)
        for index in range(len(lines)):
            with self.subTest(truncated_at=index), self.assertRaises(AssertionError):
                self.ui.instrumentation_result(''.join(lines[:index]) + final)
        for malformed in (final, self.output(count='6'), self.output(code='0'),
                          self.output().replace('passed:', 'failed:')):
            with self.subTest(final=malformed[-180:]), self.assertRaises(AssertionError):
                self.ui.instrumentation_result(malformed)

    def test_android_progress_refuses_unknown_duplicate_reordered_oversize_or_forged_frames(self):
        self.ui.select_suite(["--approval-choices"])
        text = self.output()
        changes = [
            text.replace('zerus_approval_progress=', 'stream=', 1),
            text.replace('|ON_START|', '|UNKNOWN|', 1),
            text.replace('|ON_START|-|0|', '|ON_START|countdown_once|0|', 1),
            text.replace('|STORE_BEGIN|-|0|', '|STORE_BEGIN|-|1|', 1),
            text.replace('=1|1|', '=1|01|', 1),
            text.replace('=1|2|', '=1|3|', 1),
            text.replace('|1002\n', '|0\n', 1),
            text.replace('|1001\n', '|9223372036854775808\n', 1),
            text.replace('INSTRUMENTATION_STATUS_CODE: 1', 'INSTRUMENTATION_STATUS_CODE: -1', 1),
            text.replace('INSTRUMENTATION_STATUS_CODE: 1\n', '', 1),
            text.replace('=1|1|', '=1|1|' + 'x' * 161, 1),
            text.replace('|1001\n', '|1001\nINSTRUMENTATION_RESULT: approval_cases=7\n', 1),
        ]
        first = '\n'.join(text.splitlines()[:2]) + '\n'
        changes.extend((first + text, text + first, text.replace(first, first + 'INSTRUMENTATION_STATUS_CODE: 1\n', 1)))
        for value in changes:
            self.assertNotEqual(value, text)
            with self.subTest(prefix=value[:180]), self.assertRaises(AssertionError):
                self.ui.instrumentation_result(value)

    def test_android_streaming_progress_is_bounded_passive_and_preserves_all_original_fixture_bytes(self):
        source = original_review_fixture()
        pattern = re.compile(r'^([ ]*)// BEGIN APPROVAL_PROGRESS ([a-z-]+)\n[\s\S]*?^\1// END APPROVAL_PROGRESS \2\n', re.M)
        self.assertEqual([match[2] for match in pattern.finditer(source)], ['definition', 'case-boundary', 'on-start',
            'store-begin', 'store-ready', 'bootstrap-begin', 'activity-launch-begin', 'activity-launch-ready', 'bootstrap-ready'])
        # Baseline includes guarded preparation and passive lifecycle checkpoints; progress stripping stays exact.
        self.assertEqual(hashlib.sha256(restore_compose_role_children(pattern.sub('', source)).encode()).hexdigest(), '7c2010d0dda27284e44ba3bab0167446f4f0371da36ba4e17c21a0cba539f4da')
        definition = source.split('// BEGIN APPROVAL_PROGRESS definition\n', 1)[1].split('// END APPROVAL_PROGRESS definition', 1)[0]
        self.assertIn('check(progressSequence in 0 until 32)', definition)
        self.assertIn('check(record.length <= 160)', definition)
        self.assertIn('sendStatus(1, Bundle().apply { putString("zerus_approval_progress", record) })', definition)
        self.assertEqual(source.count('sendStatus('), 1)
        for forbidden in ('main {', 'runOnMainSync', 'performAction', 'injectInputEvent', 'startActivity', 'model.', 'pending', 'postStage', 'Thread(', 'delay(', 'sleep('):
            self.assertNotIn(forbidden, definition)
        self.assertEqual(re.findall(r'progress\(ProgressStage\.([A-Z_]+)\)', source),
            ['ON_START', 'STORE_BEGIN', 'STORE_READY', 'BOOTSTRAP_BEGIN', 'ACTIVITY_LAUNCH_BEGIN', 'ACTIVITY_LAUNCH_READY', 'BOOTSTRAP_READY'])
        runner = (ROOT / 'scripts/ci/android-project-ui.py').read_text()
        run = runner.split('    def run(name, argv, cap=60, stdin=None):', 1)[1].split('    def spawn(', 1)[0]
        self.assertIn('with out.open("xb") as stdout, err.open("xb") as stderr:', run)
        self.assertIn('stdout=stdout, stderr=stderr,', run)
        self.assertNotIn('PIPE', run)
        self.assertNotIn('except', run)
        self.assertIn('[*device, "am", "instrument", "-w", "-r", "app.zerus.mobile.test/" + RUNNER], 240)', runner)

    def test_lifecycle_read_checkpoints_are_bounded_passive_and_preserve_every_original_read(self):
        source = original_review_fixture()
        pattern = re.compile(r'^([ ]*)// BEGIN LIFECYCLE_READ_DIAGNOSTIC ([a-z-]+)\n[\s\S]*?^\1// END LIFECYCLE_READ_DIAGNOSTIC \2\n', re.M)
        names = ['definition', 'resumed-main-returned', 'body-restore-begin', 'body-restore-passed',
                 'confirm-retire-begin', 'confirm-retire-passed', 'disabled-send-begin', 'disabled-send-passed',
                 'primary-failure-capture-begin', 'finally-begin']
        self.assertEqual([match[2] for match in pattern.finditer(source)], names)
        self.assertEqual(hashlib.sha256(restore_compose_role_children(pattern.sub('', source)).encode()).hexdigest(),
                         '383010e1e8d816cc7e98d85df737d0e939074d84ddbbbbc67bb0302ceb83e2ff')
        stages = ['RESUMED_MAIN_RETURNED', 'BODY_RESTORE_BEGIN', 'BODY_RESTORE_PASSED',
                  'CONFIRM_RETIRE_BEGIN', 'CONFIRM_RETIRE_PASSED', 'DISABLED_SEND_BEGIN', 'DISABLED_SEND_PASSED',
                  'PRIMARY_FAILURE_CAPTURE_BEGIN', 'FINALLY_BEGIN']
        self.assertEqual(re.findall(r'lifecycleCheckpoint\(LifecycleCheckpoint\.([A-Z_]+)\)', source), stages)
        definition = pattern.search(source)[0]
        self.assertIn('if (lifecycleCheckpointUnknown) return', definition)
        self.assertIn('if (lifecycleCheckpointCount >= 64)', definition)
        self.assertIn('lifecycleCheckpointCount++', definition)
        self.assertEqual(definition.count('lifecycleCheckpointUnknown = true'), 2)
        for outcome in ('UNKNOWN_CAP', 'UNKNOWN_FAULT'):
            self.assertEqual(definition.count('phase("lifecycle-checkpoint:' + outcome + '")'), 1)
        for forbidden in ('main {', 'runOnMainSync', 'performAction', 'injectInputEvent', 'startActivity',
                          'model.', 'sendStatus', 'Thread(', 'delay(', 'sleep(', 'await(', 'synchronized('):
            self.assertNotIn(forbidden, definition)
        body = source.split('await("same current card body restored") {', 1)[1].split('\n                }', 1)[0]
        self.assertNotIn('lifecycleCheckpoint(', body)
        self.assertIn('await("background retires confirmation") { visible("Confirm this option?") == null }', source)
        for literal, before, after in [
            ('await("same current card body restored")', 'BODY_RESTORE_BEGIN', 'BODY_RESTORE_PASSED'),
            ('await("background retires confirmation")', 'CONFIRM_RETIRE_BEGIN', 'CONFIRM_RETIRE_PASSED'),
            ('check(count() == 4 && !button("Send answer").isEnabled)', 'DISABLED_SEND_BEGIN', 'DISABLED_SEND_PASSED')]:
            position = source.index(literal, source.index('lifecycleCheckpoint(LifecycleCheckpoint.RESUMED_MAIN_RETURNED)'))
            self.assertLess(source.index('lifecycleCheckpoint(LifecycleCheckpoint.' + before + ')'), position)
            self.assertLess(position, source.index('lifecycleCheckpoint(LifecycleCheckpoint.' + after + ')'))
        failure = source.split('            primaryFailure = error\n', 1)[1]
        self.assertLess(failure.index('lifecycleCheckpoint(LifecycleCheckpoint.PRIMARY_FAILURE_CAPTURE_BEGIN)'),
                        failure.index('failureEvidence = modelEvidence('))
        cleanup = failure.split('        } finally {\n', 1)[1]
        self.assertLess(cleanup.index('lifecycleCheckpoint(LifecycleCheckpoint.FINALLY_BEGIN)'),
                        cleanup.index('main { touchTrace?.close() }'))

    def test_unknown_runner_and_extra_arguments_refuse_before_runtime(self):
        for arguments in (["other"], ["--approval-choices", "extra"], ["--approval-choices", "--approval-choices"]):
            with self.subTest(arguments=arguments), self.assertRaises(AssertionError):
                self.ui.select_suite(arguments)

    def test_android_runner_case_literal_matches_admitted_inventory(self):
        self.ui.select_suite(["--approval-choices"])
        source = original_review_fixture()
        literal = re.search(r"val CASES = listOf\(([^\n]+)\)", source)[1]
        self.assertEqual(re.findall(r'"([a-z_]+)"', literal), self.ui.CASES.split(","))

    def test_simulator_project_references_exact_production_sources_and_local_package(self):
        ios = module("ios_approval_ui_project", "scripts/ci/ios-approval-ui-project.py")
        with tempfile.TemporaryDirectory() as folder:
            project = ios.generate(ROOT, Path(folder) / "new")
            body = (project / "project.pbxproj").read_text()
            self.assertIn(str(ROOT / "mobile/ios/Zerus/Conversation/QuestionCard.swift"), body)
            self.assertIn(str(ROOT / "mobile/ios/Zerus/Common/MarkdownView.swift"), body)
            self.assertIn(str(ROOT / "mobile/ios/ZerusKit"), body)
            self.assertNotIn("App/ZerusApp.swift", body)
            self.assertNotIn("App/AppModel.swift", body)
            self.assertNotIn("CODE_SIGNING_ALLOWED = YES", body)
            self.assertIn("CODE_SIGNING_ALLOWED = NO", body)
            self.assertIn("SUPPORTED_PLATFORMS = iphonesimulator", body)
            self.assertIn("ONLY_ACTIVE_ARCH = YES", body)
            scheme = ET.parse(project / "xcshareddata/xcschemes/ApprovalUIFixture.xcscheme").getroot()
            testable = scheme.findall("./TestAction/Testables/TestableReference")
            self.assertEqual(len(testable), 1)
            self.assertEqual(testable[0].get("skipped"), "NO")
            self.assertEqual(testable[0].find("BuildableReference").get("BlueprintName"), "ApprovalUIFixtureTests")
            with self.assertRaises(FileExistsError):
                ios.generate(ROOT, project.parent)

    def test_fixture_contains_seven_distinct_native_tests_and_no_live_client(self):
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        self.assertEqual(len(set(re.findall(r"func (test\w+)\(", source))), 7)
        app = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalUIFixtureApp.swift").read_text()
        self.assertNotIn("RelayClient(", app)
        self.assertNotIn("KeychainConnectionStore(", app)
        self.assertNotIn("URLSession(", app)

    def test_notification_header_is_fixed_fixture_only_and_bound_in_both_unsigned_targets(self):
        ios = module("ios_approval_ui_project", "scripts/ci/ios-approval-ui-project.py")
        header = "mobile/ios/ApprovalUIFixture/ApprovalUIFixture-Bridging-Header.h"
        raw = (ROOT / header).read_bytes()
        self.assertEqual(raw.decode().splitlines(), [
            "// Public Apple notification declarations for the disposable fixture and its UI tests only.",
            "#include <notify.h>"])
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "new"
            project = ios.generate(ROOT, output)
            body = (project / "project.pbxproj").read_text()
            declaration = 'SWIFT_OBJC_BRIDGING_HEADER = ' + json.dumps(str(ROOT / header)) + ';'
            self.assertEqual(body.count(declaration), 2)
            for config in ("A00000000000000000000017", "A00000000000000000000018"):
                settings = body.split(config + " = { isa = XCBuildConfiguration;", 1)[1].split("\n", 1)[0]
                self.assertIn(declaration, settings)
                self.assertIn("CODE_SIGNING_ALLOWED = NO", settings)
            identities = json.loads((output / "source-identities.json").read_text())["sources"]
            self.assertEqual(identities[header], hashlib.sha256(raw).hexdigest())
            self.assertNotIn("@_silgen_name", body)

    def test_ios_result_requires_each_original_native_method_once(self):
        ios = module("ios_approval_ui", "scripts/ci/ios-approval-ui.py")
        raw = "\n".join(f"Test Case '-[ApprovalUIFixtureTests.ApprovalChoicesUITests {name}]' started.\n"
                        f"Test Case '-[ApprovalUIFixtureTests.ApprovalChoicesUITests {name}]' passed (3.123 seconds)."
                        for name in ios.CASES) + "\n** TEST SUCCEEDED **\n"
        self.assertEqual(len(ios.results(raw)["passed"]), 7)
        for broken in (raw.replace(ios.CASES[0], "testUnreviewed"), raw + raw,
                       raw.replace("passed (3.123 seconds).", "skipped (3.123 seconds).", 1),
                       raw.replace("TEST SUCCEEDED", "TEST FAILED")):
            with self.subTest(broken=broken), self.assertRaises(AssertionError):
                ios.results(broken)
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        self.assertEqual(set(re.findall(r"func (test\w+)\(", source)), set(ios.CASES))

    def test_only_the_exact_created_simulator_can_be_cleaned(self):
        ios = module("ios_approval_ui", "scripts/ci/ios-approval-ui.py")
        row = {"udid": "owned", "name": "fixture", "state": "Booted"}
        inventory = {"devices": {"runtime": [row]}}
        self.assertEqual(ios.owned_device(inventory, "runtime", "owned", "fixture"), row)
        for runtime, identifier, name in (("other-runtime", "owned", "fixture"), ("runtime", "other", "fixture"), ("runtime", "owned", "other")):
            with self.subTest(runtime=runtime, identifier=identifier, name=name), self.assertRaises(AssertionError):
                ios.owned_device(inventory, runtime, identifier, name)
        with self.assertRaises(AssertionError):
            ios.owned_device({"devices": {"runtime": [row, row]}}, "runtime", "owned", "fixture")

    def test_ios_pr_uses_exact_head_and_fixed_reviewed_inputs_without_manual_fallback(self):
        workflow = (ROOT / ".github/workflows/ios-approval-ui.yml").read_text()
        self.assertIn("pull_request:\n    branches: [main]", workflow)
        self.assertIn("github.event_name == 'pull_request' && github.event.pull_request.head.sha || inputs.source_sha", workflow)
        self.assertIn("ref: ${{ env.EXPECTED_SOURCE }}", workflow)
        for value, manual in (("Xcode_26.3.app", "xcode_application"), ("17C529", "xcode_build"),
                              ("com.apple.CoreSimulator.SimRuntime.iOS-26-2", "runtime_identifier"),
                              ("com.apple.CoreSimulator.SimDeviceType.iPhone-16", "device_type_identifier")):
            self.assertIn("github.event_name == 'pull_request' && '" + value + "' || inputs." + manual, workflow)
        for path in (".github/workflows/ios-approval-ui.yml", "mobile/ios/ApprovalUIFixture/**", "scripts/ci/ios-approval-ui*.py",
                     "tests/test_approval_ui_harness.py", "mobile/ios/Zerus/Conversation/QuestionCard.swift", "mobile/ios/Zerus/App/Theme.swift",
                     "mobile/ios/Zerus/Common/MarkdownView.swift", "mobile/ios/ZerusKit/**",
                     "mobile/ios/scripts/build-replica.sh", "crates/replica/**", "crates/replica-ffi/**"):
            self.assertIn("      - '" + path + "'", workflow)
        self.assertIn("permissions:\n  contents: read", workflow)
        self.assertIn("cache-mode: none", workflow)
        self.assertIn('test "$RUNNER_ENVIRONMENT" = github-hosted', workflow)
        self.assertNotIn("secrets.", workflow)

    def test_ios_native_fixture_builds_the_local_replica_before_xcode(self):
        workflow = (ROOT / ".github/workflows/ios-approval-ui.yml").read_text()
        steps = re.split(r"(?m)^(?=      - )", workflow.split("\n    steps:\n", 1)[1])[1:]
        install = next(step for step in steps if "run: rustup toolchain install" in step)
        build = next(step for step in steps if "bash mobile/ios/scripts/build-replica.sh" in step)
        campaign = next(step for step in steps if "run: python3 -I -B scripts/ci/ios-approval-ui.py" in step)
        self.assertLess(steps.index(install), steps.index(build))
        self.assertLess(steps.index(build), steps.index(campaign))
        targets = "aarch64-apple-ios,aarch64-apple-ios-sim,x86_64-apple-ios,aarch64-apple-darwin,x86_64-apple-darwin"
        self.assertIn("rustup toolchain install 1.99.1 --profile minimal --target " + targets, install)
        replica = (ROOT / "mobile/ios/scripts/build-replica.sh").read_text()
        self.assertEqual(re.search(r"^targets=\(([^\n]+)\)$", replica, re.M)[1].split(), targets.split(","))
        self.assertIn('CARGO_BUILD_JOBS: "2"', build)
        self.assertIn('CARGO_PROFILE_RELEASE_LTO: "false"', build)
        self.assertIn('CARGO_PROFILE_RELEASE_CODEGEN_UNITS: "16"', build)
        self.assertIn("run: RUSTUP_TOOLCHAIN=1.99.1 bash mobile/ios/scripts/build-replica.sh\n", build)
        # Preparation does not renew or enlarge the owned simulator campaign's limits.
        self.assertIn("    timeout-minutes: 25\n", workflow)
        self.assertIn("        timeout-minutes: 3\n", install)
        self.assertIn("        timeout-minutes: 10\n", build)
        for step in (install, build):
            for forbidden in ("continue-on-error", "if:", "secrets.", "--bindings-only", "cache", "DEVELOPER_DIR"):
                self.assertNotIn(forbidden, step)

    def test_non_arm64_process_refuses_before_allocation_or_tool_execution(self):
        ios = module("ios_approval_ui", "scripts/ci/ios-approval-ui.py")
        with patch.object(ios.sys, "platform", "darwin"), patch.dict(os.environ, {"RUNNER_ENVIRONMENT": "github-hosted"}, clear=True), \
                patch.object(ios.os, "uname", return_value=SimpleNamespace(machine="x86_64"), create=True), \
                patch.object(ios.Path, "mkdir") as allocate, patch.object(ios.subprocess, "Popen") as launch:
            with self.assertRaisesRegex(AssertionError, "reviewed hosted process architecture"):
                ios.main()
            allocate.assert_not_called()
            launch.assert_not_called()

    def test_hosted_apple_environment_preserves_only_valid_required_account_values(self):
        ios = module("ios_approval_ui", "scripts/ci/ios-approval-ui.py")
        valid = {"HOME": "/tmp/example-home", "TMPDIR": "/private/tmp/example-hosted/", "USER": "hosted", "LOGNAME": "hosted"}
        self.assertEqual(ios.hosted_user_environment({**valid, "UNRELATED": "discarded"}), valid)
        for key in valid:
            missing = dict(valid); missing.pop(key)
            with self.subTest(missing=key), self.assertRaises(KeyError):
                ios.hosted_user_environment(missing)
            for value in ("", "bad\x00value", "bad\nvalue", "x" * 4097, None):
                with self.subTest(key=key, value=value), self.assertRaises(AssertionError):
                    ios.hosted_user_environment({**valid, key: value})
        for changes in ({"HOME": "relative"}, {"TMPDIR": "relative"}, {"USER": "other"},
                        {"LOGNAME": "other"}, {"USER": "bad/name", "LOGNAME": "bad/name"}):
            with self.subTest(changes=changes), self.assertRaises(AssertionError):
                ios.hosted_user_environment({**valid, **changes})
        source = (ROOT / "scripts/ci/ios-approval-ui.py").read_text()
        self.assertIn('run("runtimes", ["/usr/bin/xcrun", "simctl", "list", "runtimes", "--json"], 120)', source)
        self.assertIn('run("cleanup-identity", ["/usr/bin/xcrun", "simctl", "list", "devices", "--json"], 15, True)', source)
        self.assertIn("end = time.monotonic() + 900", source)
        self.assertIn("phase_end = enter_cleanup() if cleaning else end - 60", source)

    def test_android_failure_diagnostics_stay_owned_bounded_and_cannot_replace_failure(self):
        self.ui.select_suite(["--approval-choices"])
        device = ["/fixture/adb", "-H", "127.0.0.1", "-P", "5037", "-s", self.ui.SERIAL, "shell"]
        calls = []
        def capture(name, command, cap):
            calls.append((name, command, cap))
        def live():
            calls.append("owned-alive")
        self.assertEqual(self.ui.failure_diagnostics(capture, live, device)["status"], "captured")
        self.assertEqual(calls, ["owned-alive", ("failure-logcat", [*device, "logcat", "-d", "-b", "crash", "-b", "main", "-b", "system", "-t", "400",
                                                                 "ZerusApprovalUI:I", "AndroidRuntime:E", "*:S"], 5)])
        for stage in ("ownership", "capture"):
            calls.clear()
            def refused():
                raise TimeoutError("same original end")
            def capture_refused(*arguments):
                calls.append("attempted-once"); raise OSError("diagnostic unavailable")
            result = self.ui.failure_diagnostics(capture_refused, refused if stage == "ownership" else live, device)
            self.assertEqual(result["status"], "unknown")
            self.assertEqual(result["error"], "TimeoutError" if stage == "ownership" else "OSError")
            self.assertEqual(calls, [] if stage == "ownership" else ["owned-alive", "attempted-once"])
        source = (ROOT / "scripts/ci/android-project-ui.py").read_text()
        self.assertIn('if RESULT_NAMESPACE == "approval" and instrumentation_started:', source)
        self.assertIn('diagnostics = failure_diagnostics(run, alive, device)\n        raise', without_android_focus_diagnostics(source))
        self.assertIn('"primary_failure": primary_failure, "diagnostics": diagnostics', source)
        self.assertIn('end = time.monotonic() + 1200', source)
        self.assertIn('remaining = min(cap, end - time.monotonic() - 20)', source)
        self.assertIn('"app.zerus.mobile.test/" + RUNNER], 240)', source)
        with self.assertRaises(AssertionError):
            self.ui.instrumentation_result("INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n")
        fixture = original_review_fixture()
        expected = [(event, name) for name in self.ui.CASES.split(",") for event in ("begin", "complete")]
        self.assertEqual(re.findall(r'boundary\("(begin|complete)", "([a-z_]+)"\)', fixture), expected)
        self.assertEqual(re.findall(r'cases \+= "([a-z_]+)"', fixture), self.ui.CASES.split(","))

    def test_native_preparation_reverse_rejects_changed_missing_or_duplicate_regions(self):
        source = (ROOT / "scripts/ci/android-project-ui.py").read_text()
        without_native_tool_preparation(source)
        begin = source.index("# BEGIN NATIVE_TOOL_PREPARATION environment")
        end = source.index("# END NATIVE_TOOL_PREPARATION environment\n", begin) + len("# END NATIVE_TOOL_PREPARATION environment\n")
        region = source[begin:end]
        for altered in (source.replace(region, region.replace('"CARGO_BUILD_JOBS": "2"', '"CARGO_BUILD_JOBS": "8"')),
                        source.replace(region, ""), source.replace(region, region + region)):
            self.assertNotEqual(altered, source)
            with self.assertRaises(AssertionError):
                without_native_tool_preparation(altered)

    def test_disclosure_idle_projection_refuses_changed_missing_or_duplicate_observation(self):
        source = (ROOT / _focus_projection.FIXTURE).read_text()
        original = _focus_projection.reverse_scroll_idle(source)
        self.assertNotIn('uiAutomation.waitForIdle(500, 2000)', original)
        # Pre-tap observation may not remove or repeat any original touch/case assertion.
        old_tap = original.split('    private fun tap(label: String)', 1)[1].split('    private fun reveal(', 1)[0]
        new_tap = source.split('    private fun tap(label: String)', 1)[1].split('    private fun reveal(', 1)[0]
        self.assertEqual(old_tap, new_tap)
        for altered in (source.replace('uiAutomation.waitForIdle(500, 2000)', 'uiAutomation.waitForIdle(0, 2000)'),
                        source.replace('        var stableBounds: Rect? = null\n', ''),
                        source.replace('        var stableBounds: Rect? = null\n', '        var stableBounds: Rect? = null\n' * 2)):
            with self.assertRaises(AssertionError):
                _focus_projection.reverse_scroll_idle(altered)

    def test_android_focus_diagnostics_project_to_exact_reviewed_driver_and_unchanged_ui(self):
        source = (ROOT / "scripts/ci/android-project-ui.py").read_text()
        self.assertEqual(hashlib.sha256(without_android_focus_diagnostics(without_native_tool_preparation(source)).encode()).hexdigest(),
                         "49c3814a3ef8cdb7be01e3a4792a6f5045559fb7c89db87fd53784feb4e76450")
        for path, digest in {
            "mobile/android/app/src/androidTest/java/app/zerus/mobile/ApprovalChoicesSmokeInstrumentation.kt": "73cee450f08e33c007ad6f3188a6a69135674e2503f80ea48582ba8aa394a9f7",
            "mobile/android/app/src/main/java/app/zerus/mobile/QuestionUiProbe.kt": "beb6a3caf6ae07ca3a062f4f30286aad3b19b71427eed7c9e8d662f7d5ae85be",
            "mobile/android/app/src/main/java/app/zerus/mobile/QuestionUi.kt": "53816da954ccfc33a8a64a72f5d5930b286f51b7586cc577bbcb4e2762317cb3",
        }.items():
            raw = (ROOT / path).read_text()
            if path in BASELINES: raw = reverse_focus_wait(path, raw)
            self.assertEqual(hashlib.sha256(raw.encode()).hexdigest(), digest, path)
        tree = ast.parse(source)
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        handler = next(node for node in main.body if isinstance(node, ast.Try)).handlers[0]
        calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
                 and isinstance(node.func, ast.Name) and node.func.id == "focus_failure_diagnostics"]
        self.assertEqual(len(calls), 1)
        guarded = next(node for node in handler.body if isinstance(node, ast.If))
        self.assertEqual(ast.unparse(guarded.test), "RESULT_NAMESPACE == 'approval' and instrumentation_started")
        self.assertIn(calls[0], list(ast.walk(guarded)))
        self.assertEqual(ast.unparse(handler.body[-1]), "raise")

    def test_android_focus_capture_has_one_owned_filtered_read_and_no_new_clock(self):
        calls = []
        device = ["/fixture/adb", "-H", "127.0.0.1", "-P", "5037", "-s", self.ui.SERIAL, "shell"]
        def capture(name, command, cap):
            calls.append((name, command, cap))
            return "--------- beginning of events\n   101.613   500   501 I input_focus: [Focus entering fixture-window,reason=window-change]\n"
        result = self.ui.focus_failure_diagnostics(capture, lambda: calls.append("owned-alive"), device)
        self.assertEqual(result["status"], "captured")
        self.assertEqual(result["focus_records"], 1)
        self.assertEqual(calls, ["owned-alive", ("failure-focus-events", [*device, "logcat", "-d", "-b", "events",
            "-v", "threadtime", "-v", "monotonic", "-v", "printable", "-t", "128", "input_focus:I", "wm_on_resume_called:I",
            "wm_on_paused_called:I", "wm_on_stop_called:I", "wm_on_top_resumed_gained_called:I", "wm_on_top_resumed_lost_called:I", "*:S"], 5)])
        source = (ROOT / "scripts/ci/android-project-ui.py").read_text()
        definition = source.split("# BEGIN APPROVAL_FOCUS_DIAGNOSTIC definition\n", 1)[1].split("# END APPROVAL_FOCUS_DIAGNOSTIC definition", 1)[0]
        for forbidden in ("time.", "sleep(", "while ", "dumpsys", "executeShellCommand", "shell=True", "input tap", "serviceInfo", "instrumentation_result("):
            self.assertNotIn(forbidden, definition)

    def test_android_focus_parser_keeps_tail_metadata_without_claiming_an_owner(self):
        text = ("--------- beginning of events\n"
                "   101.481   500   501 I wm_on_resume_called: [42,app.zerus.mobile.MainActivity,RESUME,0]\n"
                "   101.613   500   501 I input_focus: [Focus leaving fixture-window,reason=window-change]\n"
                "   101.969   500   501 I input_focus: [Focus entering fixture-window,reason=window-change]\n")
        result = self.ui.focus_event_observation(text)
        self.assertEqual((result["records"], result["focus_records"], result["first_monotonic_ms"], result["last_monotonic_ms"]), (3, 2, 101481, 101969))
        self.assertEqual(result["tags"]["wm_on_resume_called"], 1)
        self.assertNotIn("owner", result)
        self.assertIn("no complete-history or focus-owner claim", result["scope"])
        for empty in ("", "--------- beginning of events\n"):
            result = self.ui.focus_event_observation(empty)
            self.assertEqual((result["status"], result["records"], result["focus_records"]), ("empty", 0, 0))

    def test_android_focus_parser_refuses_unknown_overflow_or_reordered_metadata(self):
        line = "   101.613   500   501 I input_focus: [Focus entering fixture-window,reason=window-change]\n"
        self.assertEqual(self.ui.focus_event_observation(line * 128)["records"], 128)
        for invalid in (line * 129, "x" * 65537, "x" * 1025, line.replace("input_focus", "unreviewed_tag"),
                        line.replace(" I ", " V "), line.replace("[Focus", "\x00[Focus"), line.replace("[Focus", "Focus"),
                        line.replace("fixture-window", "x" * 769), line + line.replace("101.613", "100.999"),
                        line + "--------- beginning of events\n", "--------- beginning of events\n" * 2):
            with self.subTest(invalid=invalid[:60]), self.assertRaises(AssertionError):
                self.ui.focus_event_observation(invalid)

    def test_android_focus_failure_keeps_raw_primary_error_and_attempts_each_capture_once(self):
        source = (ROOT / "scripts/ci/android-project-ui.py").read_text()
        main = next(node for node in ast.parse(source).body if isinstance(node, ast.FunctionDef) and node.name == "main")
        handler = copy.deepcopy(next(node for node in main.body if isinstance(node, ast.Try)).handlers[0])
        wrapper = ast.Module(body=[ast.Try(body=[ast.Raise(exc=ast.Name(id="primary", ctx=ast.Load()))], handlers=[handler], orelse=[], finalbody=[])], type_ignores=[])
        code = compile(ast.fix_missing_locations(wrapper), "<exact-android-failure-handler>", "exec")
        for fault in (None, "ownership", "capture", "parse"):
            calls = []
            primary = RuntimeError("retained original instrumentation failure")
            def capture(name, command, cap):
                calls.append(name)
                if name == "failure-logcat" or fault == "capture":
                    raise OSError("capture unavailable")
                return "invalid" if fault == "parse" else "   101.613   500   501 I input_focus: [Focus entering fixture-window,reason=window-change]\n"
            def alive():
                if fault == "ownership": raise TimeoutError("same original end")
            namespace = dict(primary=primary, RESULT_NAMESPACE="approval", instrumentation_started=True, device=["owned-device"],
                run=capture, alive=alive, failure_diagnostics=self.ui.failure_diagnostics, focus_failure_diagnostics=self.ui.focus_failure_diagnostics)
            with self.subTest(fault=fault), self.assertRaises(RuntimeError) as caught:
                exec(code, namespace)
            self.assertIs(caught.exception, primary)
            self.assertEqual(namespace["primary_failure"], {"error": "RuntimeError", "message": str(primary)})
            self.assertEqual(namespace["diagnostics"]["status"], "unknown")
            self.assertEqual(calls, [] if fault == "ownership" else ["failure-logcat", "failure-focus-events"])
            self.assertEqual(namespace["diagnostics"]["focus"]["status"], "captured" if fault is None else "unknown")

    def test_ios_fixture_launch_and_cleanup_stay_inside_each_main_actor_test(self):
        ios = module("ios_approval_ui", "scripts/ci/ios-approval-ui.py")
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        self.assertIn("@MainActor final class ApprovalChoicesUITests: XCTestCase", source)
        self.assertIn("@MainActor private func launchFixture(_ mode: Mode = .normal)", source)
        self.assertNotIn("override func setUp", source)
        self.assertNotIn("override func tearDown", source)
        entries = re.findall(r"func (test\w+)\(\) \{\n        defer \{ closeFixture\(\) \}\n        launchFixture\((?:\.(?:countdown|feedback|replacement|unavailable))?\)", source)
        self.assertEqual(len(entries), 7)
        self.assertEqual(set(entries), set(ios.CASES))
        self.assertEqual(len(re.findall(r"^        launchFixture\(", source, re.M)), 7)

    def test_android_interceptor_failure_is_first_only_and_cannot_qualify_or_use_network(self):
        source = original_review_fixture()
        self.assertIn("private val interceptorFailure = AtomicReference<InterceptorFailure?>(null)", source)
        self.assertIn("interceptorFailure.compareAndSet(null, failure)", source)
        self.assertNotIn("interceptorFailure.set(", source)
        self.assertNotIn("chain.proceed(", source)
        self.assertNotIn("request.url.query", source)
        catch = source.split("            } catch (error: Throwable) {", 1)[1].split("        }.build()", 1)[0]
        self.assertIn("request.method.take(16), request.url.encodedPath.take(160)", catch)
        self.assertNotIn("request.body", catch)
        self.assertNotIn('header("Authorization")', catch)
        self.assertIn('.code(500).message("Synthetic fixture refusal")', catch)
        self.assertIn("            smoke(store)\n            requireHealthyFixture()\n            check(cases == CASES)", source)
        wait = source.split("    private fun await(", 1)[1].split("    private fun find(", 1)[0]
        self.assertEqual(wait.count("requireHealthyFixture()"), 3)
        self.assertLess(wait.index("requireHealthyFixture()"), wait.index("while (!condition())"))
        self.assertGreater(wait.rindex("requireHealthyFixture()"), wait.index("SystemClock.sleep(30)"))
        self.assertEqual(re.findall(r'(?:var guard|guard) = "([a-z-]+)"', source), [
            "request-scope", "metadata-origin", "metadata-path", "authorization", "request-json", "operation", "answer-admission", "answer-payload",
            "answer-target", "answer-receipt-identity", "answer-question-identity", "answer-list", "answer-body", "receipt"])

    def test_android_native_text_search_keeps_exact_matches_refresh_fallback_and_real_touch(self):
        source = original_review_fixture()
        self.assertIn("root.findAccessibilityNodeInfosByText(label).orEmpty().firstOrNull { it.refresh() && exactVisible(it, label) }", source)
        self.assertIn("node.isVisibleToUser &&\n        (node.text?.toString() == label || node.contentDescription?.toString() == label)", source)
        self.assertIn("return find(root) { exactVisible(it, label) }", source)
        node = source.split("    private fun node(", 1)[1].split("    private fun button(", 1)[0]
        self.assertEqual(node.count("visible(label)"), 1)
        self.assertIn("return checkNotNull(matched)", node)
        self.assertIn("check(current.refresh())", source)
        self.assertIn("check(action.isEnabled && action.isClickable)", source)
        self.assertIn("check(uiAutomation.injectInputEvent(event, true))", source)
        for phase in ("open-begin:$name", "open-ready:$name", "touch-down:$label", "touch-up:$label",
                      "disabled-assert-begin:countdown", "disabled-assert-end:countdown"):
            self.assertIn('phase("' + phase + '")', source)
        self.assertIn('check(!button("Send answer").isEnabled && count() == 0)', source)
        self.assertNotIn("setReviewDelayForTests", source)

    def test_android_failure_phase_buffer_is_bounded_and_does_not_change_pass_schema(self):
        source = original_review_fixture()
        self.assertIn("private val phases = ArrayDeque<String>()", source)
        phase = source.split("    private fun phase(", 1)[1].split("    override fun onCreate", 1)[0]
        self.assertIn(".take(160)", phase)
        self.assertIn("if (phases.size == 64) phases.removeFirst()", phase)
        self.assertIn("phases.addLast(record)", phase)
        self.assertIn('${error.stackTraceToString()}\\n${phases.joinToString("\\n")}\\n', source)
        success = source.split('            result.putString("approval_cases"', 1)[1].split('        } catch (error: Throwable)', 1)[0]
        self.assertNotIn("phases", success)
        self.assertIn('finish(Activity.RESULT_OK, result)', success)

    def test_android_early_windows_use_owned_lifecycle_and_scoped_fresh_identity_cache(self):
        source = original_review_fixture()
        button = source.split("    private fun button(", 1)[1].split("    private fun tap(", 1)[0]
        self.assertIn('earlySend?.takeIf { label == "Send answer" }', button)
        self.assertIn('return cachedSendButton(cached)', button)
        self.assertIn('check(visibleLabel == null)', button)
        self.assertIn('check(cached.current())', button)
        self.assertIn('cached.node.refresh() && cached.window >= 0 && cached.node.windowId == cached.window', button)
        self.assertIn('exactVisible(cached.node, "Send answer")', button)
        self.assertIn('var current = node(label)', button)
        helper = source.split("        fun captureEarlySend(", 1)[1].split("        try {\n            await(\"private model storage\")", 1)[0]
        for identity in ('model.selected?.target == target && model.activityVerified', 'card.id == id && card.hash == hash',
                         'card.run == target.run && card.conversation == target.conversation'):
            self.assertIn(identity, helper)
        self.assertLess(helper.index('earlySend = captured'), helper.index('sendKeyDownUpSync(KeyEvent.KEYCODE_HOME)'))
        self.assertLess(helper.index('!main { activity.hasWindowFocus() }'), helper.index('val resumeStart = SystemClock.elapsedRealtime()'))
        self.assertLess(helper.index('val resumeStart = SystemClock.elapsedRealtime()'), helper.index('targetContext.startActivity('))
        self.assertLess(helper.index('captured.begin(resumeStart)'), helper.index('targetContext.startActivity('))
        self.assertLess(helper.index('main { activity.hasWindowFocus() }'), helper.index('checkEarly()'))
        self.assertLess(helper.index('captured.bindReview()'), helper.index('checkEarly()'))
        self.assertLess(helper.index('checkEarly()'), helper.index('val elapsed = captured.elapsed()'))
        self.assertIn('elapsed >= 0 && elapsed < 3_000', helper)
        self.assertIn('finally {\n                earlySend = null', helper)
        self.assertEqual(re.findall(r'earlyReview\("([a-z]+)"\)', source), ['countdown', 'replacement', 'feedback'])
        self.assertLess(source.index('await("replacement drops old confirmation")'), source.index('earlyReview("replacement")'))
        self.assertLess(source.index('check(editor.performAction(AccessibilityNodeInfo.ACTION_FOCUS))'), source.index('earlyReview("feedback")'))
        lifecycle = source.split('boundary("begin", "lifecycle_no_submit")', 1)[1].split('boundary("complete", "lifecycle_no_submit")', 1)[0]
        self.assertNotIn('earlyReview(', lifecycle)
        self.assertEqual(lifecycle.count('sendKeyDownUpSync(KeyEvent.KEYCODE_HOME)'), 1)
        self.assertEqual(lifecycle.count('targetContext.startActivity('), 1)
        self.assertLess(lifecycle.index('val lifecycleSend = captureEarlySend()'), lifecycle.index('tap("Send answer")'))
        self.assertLess(lifecycle.index('earlySend = lifecycleSend'), lifecycle.index('sendKeyDownUpSync(KeyEvent.KEYCODE_HOME)'))
        self.assertLess(lifecycle.index('val resumeStart = SystemClock.elapsedRealtime()'), lifecycle.index('targetContext.startActivity('))
        self.assertLess(lifecycle.index('lifecycleSend.begin(resumeStart)'), lifecycle.index('targetContext.startActivity('))
        self.assertLess(lifecycle.index('lifecycleSend.bindReview()'), lifecycle.index('check(count() == 4 && !button("Send answer").isEnabled)'))
        self.assertLess(lifecycle.index('check(count() == 4 && !button("Send answer").isEnabled)'), lifecycle.index('val elapsed = '))
        self.assertIn('elapsed >= 0 && elapsed < 3_000', lifecycle)
        self.assertIn('finally {\n                earlySend = null', lifecycle)
        self.assertIn('check(count() == 4 && !button("Send answer").isEnabled)', lifecycle)
        for forbidden in ('setActivityVerified', 'setDeclaredField("activeFrameVerified"', 'setReviewDelayForTests', 'CountDownLatch'):
            self.assertNotIn(forbidden, source)

    def test_android_cached_send_rewalks_only_stale_ancestors_under_one_original_end(self):
        source = original_review_fixture()
        clock = SimpleNamespace(now=1_000)
        class Node:
            def __init__(self, *, refresh=True, parent=None, **values):
                self.__dict__.update(window=7, package='app.zerus.mobile', visible=True, label='Send answer', clickable=False,
                                     enabled=True, refreshes=0, reads=0, cost=0, fault=None, parent_fault=None)
                self.__dict__.update(values)
                self.answers = [refresh] if isinstance(refresh, bool) else list(refresh)
                self.parents = [parent] if not isinstance(parent, list) else list(parent)
            def refresh(self):
                self.refreshes += 1
                clock.now += self.cost
                if self.fault: raise self.fault
                return self.answers.pop(0) if len(self.answers) > 1 else self.answers[0]
            @property
            def parent(self):
                self.reads += 1
                if self.parent_fault: raise self.parent_fault
                return self.parents.pop(0) if len(self.parents) > 1 else self.parents[0]
        def read(anchor, **overrides):
            return cached_send_memory(source, anchor, 7, 'app.zerus.mobile', overrides.pop('end', 4_000), clock, **overrides)
        action = Node(clickable=True, enabled=False)
        stale = Node(refresh=False)
        anchor = Node(parent=[stale, action])
        self.assertEqual(read(anchor), (action, 2, 1))
        self.assertEqual(anchor.refreshes, 2)  # Once per walk; never the old second refresh in the ancestor loop.
        self.assertFalse(action.enabled)
        for change in ({'refresh': False}, {'window': 8}, {'package': 'foreign'}, {'visible': False},
                       {'label': 'Other'}, {'parent': None}, {'fault': RuntimeError('read')},
                       {'parent_fault': RuntimeError('parent-read')}):
            clock.now = 1_000
            with self.subTest(anchor=change), self.assertRaises((AssertionError, RuntimeError)):
                read(Node(**change))
        for change in ({'window': 8}, {'package': 'foreign'}, {'fault': RuntimeError('read')}):
            clock.now = 1_000
            with self.subTest(ancestor=change), self.assertRaises((AssertionError, RuntimeError)):
                read(Node(parent=Node(clickable=True, **change)))
        clock.now = 1_000
        with self.assertRaises(AssertionError): read(Node(parent=action), current_card=lambda: False)
        states = iter([True, False])
        with self.assertRaises(AssertionError): read(Node(parent=action), current_card=lambda: next(states))
        deep = action
        for _ in range(16): deep = Node(parent=deep)
        with self.assertRaises(AssertionError): read(deep)
        for anchor, end in ((Node(parent=stale), 1_015), (Node(parent=Node(clickable=True, cost=3_000)), 4_000),
                            (Node(parent=action, cost=3_000), 4_000), (Node(parent=action), 1_000),
                            (Node(parent=action), None)):
            clock.now = 1_000
            with self.subTest(end=end), self.assertRaises(AssertionError): read(anchor, end=end)
        clock.now = 1_000
        enabled, _, _ = read(Node(parent=Node(clickable=True)))
        with self.assertRaises(AssertionError): assert not enabled.enabled  # Preserve the actual early disabled oracle.
        cache = source.split('    private data class EarlySend(', 1)[1].split('    private var earlySend:', 1)[0]
        self.assertIn('private set', cache)
        self.assertLess(cache.index('check(this.resumeStart == null && resumeEnd == null)'), cache.index('this.resumeStart = resumeStart'))
        self.assertLess(cache.index('check(reviewStart == null && resumeEnd == null'), cache.index('resumeEnd = end'))
        self.assertEqual(source.count('.begin(resumeStart)'), 2)
        resolver = source.split('    private fun cachedSendButton(', 1)[1].split('    private fun tap(', 1)[0]
        for forbidden in ('node(', 'visible(', 'uiAutomation', 'await(', 'injectInputEvent', 'performAction', 'catch (', 'phase('):
            self.assertNotIn(forbidden, resolver)

    def test_android_live_anchor_preparation_preserves_the_original_fixture_and_reader(self):
        source = original_review_fixture()
        pattern = re.compile(r'^([ ]*)// BEGIN EARLY_SEND_LIVE_ANCHOR ([a-z-]+)\n[\s\S]*?^\1// END EARLY_SEND_LIVE_ANCHOR \2\n', re.M)
        self.assertEqual([match[2] for match in pattern.finditer(source)], ['state', 'preparation', 'early-review', 'lifecycle'])
        projected = pattern.sub('', source)
        constructor = 'private data class EarlySend(private val initialNode: AccessibilityNodeInfo,'
        self.assertEqual(projected.count(constructor), 1)
        projected = projected.replace(constructor, 'private data class EarlySend(val node: AccessibilityNodeInfo,')
        self.assertEqual(hashlib.sha256(restore_compose_role_children(projected).encode()).hexdigest(),
                         'adb00c075f257a89a84aef23227de0342a0e4cd103894bda923487f00fd1d480')
        reader = lambda value: value.split('    private fun cachedSendButton(', 1)[1].split('    private fun tap(', 1)[0]
        self.assertEqual(reader(source), reader(projected))
        early = source.split('        fun earlyReview(', 1)[1].split('        var primaryFailure:', 1)[0]
        self.assertEqual(early.count('prepareEarlySendAnchor(captured)'), 1)
        self.assertLess(early.index('captured.bindReview()'), early.index('prepareEarlySendAnchor(captured)'))
        self.assertLess(early.index('check(captured.current())'), early.index('prepareEarlySendAnchor(captured)'))
        self.assertLess(early.index('prepareEarlySendAnchor(captured)'), early.index('checkEarly()'))
        lifecycle = source.split('boundary("begin", "lifecycle_no_submit")', 1)[1].split('boundary("complete", "lifecycle_no_submit")', 1)[0]
        self.assertEqual(lifecycle.count('prepareEarlySendAnchor(lifecycleSend)'), 1)
        self.assertLess(lifecycle.index('disclosureScrollReady(lifecycleDisclosure)'), lifecycle.index('prepareEarlySendAnchor(lifecycleSend)'))
        self.assertLess(lifecycle.index('await("background retires confirmation")'), lifecycle.index('prepareEarlySendAnchor(lifecycleSend)'))
        self.assertLess(lifecycle.index('prepareEarlySendAnchor(lifecycleSend)'), lifecycle.index('check(count() == 4 && !button("Send answer").isEnabled)'))
        helper = source.split('    private fun prepareEarlySendAnchor(', 1)[1].split('// END EARLY_SEND_LIVE_ANCHOR preparation', 1)[0]
        self.assertEqual(helper.count('visible("Send answer")'), 1)
        for read in ('fresh.refresh()', 'fresh.windowId', 'fresh.packageName', 'fresh.isVisibleToUser',
                     'fresh.text', 'fresh.contentDescription'):
            self.assertEqual(helper.count(read), 1)
        for forbidden in ('await(', 'node(', 'catch (', 'sleep(', 'Thread(', 'performAction', 'injectInputEvent',
                          '.begin(', '.bindReview(', 'resumeEnd =', 'reviewStart =', 'boundSnapshot =', 'boundGeneration ='):
            self.assertNotIn(forbidden, helper)
        self.assertEqual(source.count('prepareEarlySendAnchor('), 3)  # One definition and the two fixed call sites.

    def test_android_live_anchor_has_a_source_bound_obsolete_node_counterexample(self):
        source = original_review_fixture()
        clock = SimpleNamespace(now=1000)
        def node(*, refresh=True, parent=None, clickable=False, enabled=True):
            return SimpleNamespace(refresh=lambda: refresh, parent=parent, clickable=clickable, enabled=enabled,
                                   window=7, package='app.zerus.mobile', visible=True, label='Send answer',
                                   text='Send answer', description=None)
        old = node(refresh=False)
        bound = object()
        cache = SimpleNamespace(node=old, window=7, end=4000, start=1000, generation=4,
                                snapshot=bound, current=lambda: True, attempted=False)
        # Controlled source stand-in: an obsolete pre-Home node refuses even though logical identity remains current.
        with self.assertRaises(AssertionError):
            cached_send_memory(source, old, cache.window, 'app.zerus.mobile', cache.end, clock)
        fresh = node(parent=node(clickable=True, enabled=False))
        queries = []
        def acquire(label):
            queries.append(label)
            return fresh
        self.assertIs(prepare_anchor_memory(source, cache, acquire, clock, 'app.zerus.mobile'), fresh)
        action, walks, sleeps = cached_send_memory(source, cache.node, cache.window, 'app.zerus.mobile', cache.end, clock)
        self.assertFalse(action.enabled)
        self.assertEqual((walks, sleeps, queries), (1, 0, ['Send answer']))
        self.assertEqual((cache.window, cache.start, cache.end, cache.generation), (7, 1000, 4000, 4))
        self.assertIs(cache.snapshot, bound)
        with self.assertRaisesRegex(AssertionError, 'preparation repeated'):
            prepare_anchor_memory(source, cache, acquire, clock, 'app.zerus.mobile')
        self.assertEqual(queries, ['Send answer'])
        # A live enabled action still fails the original negative disabled oracle.
        enabled = node(parent=node(clickable=True))
        cache = SimpleNamespace(node=old, window=7, end=4000, current=lambda: True, attempted=False)
        prepare_anchor_memory(source, cache, lambda _: enabled, clock, 'app.zerus.mobile')
        with self.assertRaises(AssertionError):
            assert not cached_send_memory(source, cache.node, 7, 'app.zerus.mobile', 4000, clock)[0].enabled

    def test_android_live_anchor_refuses_missing_foreign_late_faulted_or_repeated_preparation(self):
        source = original_review_fixture()
        clock = SimpleNamespace(now=1000)
        def candidate(**changes):
            value = SimpleNamespace(refresh=lambda: True, window=7, package='app.zerus.mobile', visible=True,
                                    text='Send answer', description=None)
            value.__dict__.update(changes)
            return value
        rows = [
            ('missing', lambda: None, {}, 'label is unavailable'),
            ('obsolete', lambda: candidate(refresh=lambda: False), {}, 'label is obsolete'),
            ('window', lambda: candidate(window=8), {}, 'window changed'),
            ('unknown-window', candidate, {'window': -1}, 'window changed'),
            ('package', lambda: candidate(package='foreign'), {}, 'package changed'),
            ('invisible', lambda: candidate(visible=False), {}, 'not visible'),
            ('label', lambda: candidate(text='Other'), {}, 'label changed'),
            ('unarmed', candidate, {'end': None}, ''),
            ('current-before', candidate, {'current': lambda: False}, 'identity changed before'),
            ('expired-before', candidate, {'end': 1000}, 'expired before'),
        ]
        for name, build, changes, reason in rows:
            clock.now = 1000
            old = object()
            cache = SimpleNamespace(node=old, window=7, end=4000, current=lambda: True, attempted=False)
            cache.__dict__.update(changes)
            queries = []
            def acquire(label):
                queries.append(label)
                return build()
            with self.subTest(refusal=name), self.assertRaisesRegex(AssertionError, reason):
                prepare_anchor_memory(source, cache, acquire, clock, 'app.zerus.mobile')
            self.assertIs(cache.node, old)
            self.assertEqual(len(queries), 0 if name in ('unarmed', 'current-before', 'expired-before') else 1)
            with self.assertRaisesRegex(AssertionError, 'preparation repeated'):
                prepare_anchor_memory(source, cache, acquire, clock, 'app.zerus.mobile')
            self.assertLessEqual(len(queries), 1)
        for stage in ('query-error', 'refresh-error', 'query-expired', 'refresh-expired', 'current-after-query', 'current-after-validation'):
            clock.now = 1000
            old = object()
            cache = SimpleNamespace(node=old, window=7, end=4000, current=lambda: True, attempted=False)
            if stage.startswith('current-after'):
                answers = iter([True, False] if stage.endswith('query') else [True, True, False])
                cache.current = lambda: next(answers)
            queries = []
            def fault(): raise RuntimeError('owned read failed')
            def refresh():
                if stage == 'refresh-error': fault()
                if stage == 'refresh-expired': clock.now = 4000
                return True
            def acquire(label):
                queries.append(label)
                if stage == 'query-error': fault()
                if stage == 'query-expired': clock.now = 4000
                return candidate(refresh=refresh)
            with self.subTest(refusal=stage), self.assertRaises((AssertionError, RuntimeError)):
                prepare_anchor_memory(source, cache, acquire, clock, 'app.zerus.mobile')
            self.assertEqual(queries, ['Send answer'])
            self.assertIs(cache.node, old)
            self.assertEqual(cache.end, 4000)
        # Exact content-description identity remains an allowed label shape; text takes the original short-circuit path.
        clock.now = 1000
        cache = SimpleNamespace(node=object(), window=7, end=4000, current=lambda: True, attempted=False)
        fresh = candidate(text='Other', description='Send answer')
        self.assertIs(prepare_anchor_memory(source, cache, lambda _: fresh, clock, 'app.zerus.mobile'), fresh)

    def test_android_early_clock_binds_once_to_the_current_owned_production_start(self):
        source = original_review_fixture()
        clock = SimpleNamespace(now=1000)
        snapshot = SimpleNamespace(generation=1, start=500, eligible=True)
        alive = True
        ready = True
        def observe(after):
            assert alive  # Closed/faulted/foreign/missing/current-card mismatch is a hard failure, never a fallback.
            if after is not None and not ready: return None
            return snapshot
        def fresh():
            clock.now = 1000
            snapshot.generation, snapshot.start, snapshot.eligible = 1, 500, True
            return review_clock_memory(source, observe, clock)
        bound = fresh(); bound.begin(1000)
        clock.now = 4214  # The previous fixture expired at 4000 despite production only just becoming eligible.
        snapshot.generation, snapshot.start = 2, 4100
        self.assertTrue(bound.bind())
        self.assertEqual(bound.state['end'], 7100)
        self.assertTrue(bound.current())
        clock.now = 4099
        self.assertFalse(bound.current())
        clock.now = 4214
        with self.assertRaises(AssertionError): bound.begin(4214)
        with self.assertRaises(AssertionError): bound.bind()
        snapshot.start = 4101
        self.assertTrue(bound.current())  # A forward reset cannot extend the immutable first window.
        self.assertEqual(bound.state['end'], 7100)
        for now in (7100, 7101):
            clock.now = now
            self.assertFalse(bound.current())
        clock.now = 4214
        snapshot.start = 4100
        for change in ({'generation': 1}, {'start': 4099}, {'start': 4215}, {'start': None}, {'eligible': False}):
            old = vars(snapshot).copy()
            snapshot.__dict__.update(change)
            with self.subTest(current=change): self.assertFalse(bound.current())
            snapshot.__dict__.update(old)
        for change in ({'generation': 1}, {'start': None}, {'start': 999}, {'start': 2001}, {'eligible': False}):
            candidate = fresh(); candidate.begin(1000); clock.now = 2000
            snapshot.generation, snapshot.start, snapshot.eligible = 2, 1500, True
            snapshot.__dict__.update(change)
            with self.subTest(bind=change), self.assertRaises(AssertionError): candidate.bind()
            self.assertIsNone(candidate.state['end'])
        for now in (4500, 4501):
            candidate = fresh(); candidate.begin(1000)
            snapshot.generation, snapshot.start = 2, 1500
            clock.now = now
            with self.subTest(expired=now), self.assertRaises(AssertionError): candidate.bind()
        candidate = fresh(); candidate.begin(1000)
        alive = False
        with self.assertRaises(AssertionError): candidate.bind()
        alive = True
        candidate = fresh(); candidate.begin(1000)
        ready = False
        self.assertFalse(candidate.bind())
        self.assertIsNone(candidate.state['end'])
        ready = True
        for resume in (-1, 1001):
            with self.subTest(resume=resume), self.assertRaises(AssertionError): fresh().begin(resume)
        unknown = review_clock_memory(source, observe, clock, current_card=lambda: False)
        with self.assertRaises(AssertionError): unknown.begin(1000)
        observation = source.split('            val observation = { afterGeneration: Int? -> main {', 1)[1].split('            return EarlySend(', 1)[0]
        self.assertIn('check(currentCard())', observation)
        self.assertIn('QuestionUiProbe.contentSnapshot(checkNotNull(actionProbeSession), checkNotNull(model.conversationQuestions.singleOrNull()), afterGeneration)', observation)
        self.assertEqual(source.count('.bindReview()'), 2)
        self.assertNotIn('await(', source.split('        fun bindReview():', 1)[1].split('        fun current()', 1)[0])
        self.assertIn('await("same Activity regained focus for $name") { main { activity.hasWindowFocus() } && captured.bindReview() }', source)

    def test_android_owned_probe_snapshot_never_reenters_the_main_thread_helper(self):
        source = original_review_fixture()
        self.assertEqual(early_probe_main_depths(source), dict(currentCard=0, current=1, observation=1, currentReview=1))
        old_nested = source.replace('                check(currentCard())\n', '                check(current())\n', 1)
        self.assertNotEqual(old_nested, source)
        with self.assertRaises(AssertionError): early_probe_main_depths(old_nested)
        predicate_nested = source.replace('            fun currentCard(): Boolean {\n                val card = model.conversationQuestions.singleOrNull()',
                                          '            fun currentCard(): Boolean {\n                val card = main { model.conversationQuestions.singleOrNull() }', 1)
        self.assertNotEqual(predicate_nested, source)
        with self.assertRaises(AssertionError): early_probe_main_depths(predicate_nested)
        observation = source.split('            val observation = ', 1)[1].split('            return EarlySend(', 1)[0]
        self.assertLess(observation.index('check(currentCard())'), observation.index('QuestionUiProbe.contentSnapshot('))
        for forbidden in ('await(', 'sleep(', 'startActivity(', 'injectInputEvent', 'performAction', 'current()'):
            self.assertNotIn(forbidden, observation)

    def test_android_lifecycle_stop_requires_the_same_alive_created_activity_not_modal_focus_loss(self):
        source = original_review_fixture()
        body = source.split('boundary("begin", "lifecycle_no_submit")', 1)[1].split('boundary("complete", "lifecycle_no_submit")', 1)[0]
        stop = body.split('await("Activity stopped") { main {\n', 1)[1].split('                } }', 1)[0]
        resume = body.split('await("same Activity regained focus") { main {\n', 1)[1].split('                } && lifecycleSend.bindReview() }', 1)[0]
        def admitted(fragment, values):
            lines = [line.strip() for line in fragment.splitlines() if line.strip()]
            self.assertEqual(len(lines), 2)
            self.assertTrue(lines[0].startswith('check(') and lines[0].endswith(')'))
            expressions = [lines[0][6:-1], lines[1]]
            for expression in expressions:
                for literal, replacement in (
                    ('lifecycleActivity === activity', 'same'), ('activity.isFinishing', 'finishing'),
                    ('activity.isDestroyed', 'destroyed'), ('activity.lifecycle.currentState', 'state'),
                    ('androidx.lifecycle.Lifecycle.State.CREATED', repr('CREATED')),
                    ('androidx.lifecycle.Lifecycle.State.RESUMED', repr('RESUMED')),
                    ('activity.hasWindowFocus()', 'focus'), ('&&', ' and '), ('!', 'not ')):
                    expression = expression.replace(literal, replacement)
                if not finite_source_predicate(expression, values): return False
            return True
        modal = dict(same=True, finishing=False, destroyed=False, state='RESUMED', focus=False)
        self.assertTrue(finite_source_predicate('not focus', modal))  # The retained old focus-only predicate admitted this.
        self.assertFalse(admitted(stop, modal))
        stopped = {**modal, 'state': 'CREATED'}
        self.assertTrue(admitted(stop, stopped))
        for changed in ({'same': False}, {'finishing': True}, {'destroyed': True}, {'state': 'STARTED'},
                        {'state': 'RESUMED'}, {'state': 'DESTROYED'}, {'focus': True}):
            with self.subTest(stopped=changed): self.assertFalse(admitted(stop, {**stopped, **changed}))
        resumed = {**modal, 'focus': True}
        self.assertTrue(admitted(resume, resumed))
        for changed in ({'same': False}, {'finishing': True}, {'destroyed': True}, {'state': 'CREATED'}, {'focus': False}):
            with self.subTest(resumed=changed): self.assertFalse(admitted(resume, {**resumed, **changed}))
        self.assertIn('val lifecycleActivity = activity', body)
        for phase in ('lifecycle-before-home:', 'lifecycle-stopped:', 'lifecycle-resumed:'):
            line = next(line for line in body.splitlines() if 'phase("' + phase in line)
            self.assertIn('System.identityHashCode(activity)', line)
            self.assertIn('activity.lifecycle.currentState.name', line)
            self.assertIn('activity.hasWindowFocus()', line)
        sequence = ('phase("lifecycle-before-home:', 'sendKeyDownUpSync(KeyEvent.KEYCODE_HOME)',
                    'await("Activity stopped")', 'phase("lifecycle-stopped:', 'val resumeStart = ',
                    'targetContext.startActivity(', 'await("same Activity regained focus")', 'phase("lifecycle-resumed:',
                    'await("same current card body restored")', 'await("background retires confirmation")',
                    'check(count() == 4 && !button("Send answer").isEnabled)', 'val elapsed = ',
                    'elapsed >= 0 && elapsed < 3_000')
        self.assertEqual([body.index(item) for item in sequence], sorted(body.index(item) for item in sequence))
        self.assertEqual(body.count('sendKeyDownUpSync(KeyEvent.KEYCODE_HOME)'), 1)
        self.assertEqual(body.count('targetContext.startActivity('), 1)
        wait = source.split('    private fun await(', 1)[1].split('    private fun find(', 1)[0]
        self.assertIn('SystemClock.elapsedRealtime() + 10_000', wait)
        self.assertIn('SystemClock.sleep(30)', wait)
        self.assertNotIn('sleep(', body)

    def test_ios_controls_bind_modes_generation_first_consumption_and_checked_cleanup(self):
        host = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalUIFixtureApp.swift").read_text()
        tests = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        controls = host.split("final class FixtureControls {", 1)[1].split("// This file belongs only", 1)[0]
        self.assertIn('case beginFeedbackReview = "begin-feedback-review", replace', controls)
        self.assertIn("case .normal: []; case .feedback: [.beginFeedbackReview]; case .replacement: [.replace]", controls)
        self.assertIn("!closed, failure.isEmpty, actions.contains(action), !consumed.contains(action), tokens[action] == token", controls)
        self.assertLess(controls.index("tokens.removeValue(forKey: action)"), controls.index("guard notify_cancel(token) == 0"))
        self.assertLess(controls.index("guard notify_cancel(token) == 0"), controls.index("consumed.insert(action)"))
        self.assertIn('if notify_cancel(token) != 0 { failed("close-cancel") }', controls)
        self.assertIn('if failure.isEmpty { status = "closed" }', controls)
        self.assertIn("notify_register_dispatch(pointer, &token, DispatchQueue.main)", controls)
        self.assertIn("MainActor.assumeIsolated { self?.consume(action, token: received) }", controls)
        for forbidden in ("sendQuestion(", "callbacks.append(", "URLSession(", "FileManager(", "notify_register_signal("):
            self.assertNotIn(forbidden, controls)
        self.assertIn("let actualGeneration = UUID().uuidString", host)
        self.assertIn("generation = actualGeneration", host)
        self.assertIn('fixtureLaunch = UUID().uuidString.lowercased()', tests)
        self.assertIn('XCTAssertEqual(app.staticTexts["fixture-generation"].label, fixtureGeneration)', tests)
        self.assertIn('XCTAssertTrue(posted.insert(action).inserted, "No control replays")', tests)
        self.assertIn("notify_post($0)", tests)
        self.assertIn("Owned registration cleanup must be acknowledged", tests)
        self.assertEqual(host.count("test.example.zerus.ui."), 1)
        self.assertEqual(tests.count("test.example.zerus.ui."), 2)

    def test_ios_feedback_review_and_replacement_handshakes_preserve_real_assertions(self):
        host = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalUIFixtureApp.swift").read_text()
        tests = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        self.assertIn("available = selectedMode != .feedback", host)
        self.assertIn('"text": "Use a safer comman"', host)
        self.assertIn('question.id == "fixture-approval" && question.hash == "fixture-hash-0"', host)
        feedback = tests.split("    func testFeedbackKeyboardCannotBypassReview()", 1)[1].split("    func testBackground", 1)[0]
        for earlier, later in [('field.tap()', 'beginReview("begin-feedback-review")'), ('beginReview("begin-feedback-review")', 'field.typeText("d\\n")'),
                               ('field.typeText("d\\n")', 'let appBegan = ack('), ('let appBegan = ack(', 'count(0)'),
                               ('count(0)', 'reviewed(); field.typeText("\\n")')]:
            self.assertLess(feedback.index(earlier), feedback.index(later))
        self.assertIn("XCTAssertLessThan(earlyElapsed, 3", feedback)
        self.assertIn('answer.contains("Use a safer command")', feedback)
        self.assertIn('XCTAssertEqual(answer, "fixture-hash-0|[', feedback)
        replacement = tests.split("    func testReplacementRetiresAnOpenConfirmation()", 1)[1].split("    func testUnavailable", 1)[0]
        self.assertLess(replacement.index('XCTAssertTrue(app.buttons["Confirm"].waitForExistence'), replacement.index('post("replace")'))
        self.assertLess(replacement.index('post("replace")'), replacement.index('XCTWaiter.wait(for: [retired]'))
        self.assertIn('count(0); XCTAssertFalse(send.isEnabled)', replacement)
        self.assertNotIn('Replace in 2 seconds', host + tests)
        self.assertIn("changes the agent's permissions beyond this request.\\n\\nExact command for this session only", tests)

    def test_android_metadata_source_predicate_refuses_every_unbound_origin_and_extra_path(self):
        source = original_review_fixture()
        metadata = source.split('                if (request.method == "GET") {', 1)[1].split('                check(request.url.host == "relay.example.test"', 1)[0]
        condition = metadata.split("                    check(", 1)[1].split(')\n                    guard = "metadata-path"', 1)[0]
        fields = {"request.url.scheme": "scheme", "request.url.host": "host", "request.url.port": "port",
                  "request.url.encodedUsername": "username", "request.url.encodedPassword": "password",
                  "request.url.encodedQuery": "query", "request.url.encodedFragment": "fragment", "request.body": "body",
                  'request.header("Authorization")': "authorization"}
        for old, new in fields.items():
            condition = condition.replace(old, new)
        condition = condition.replace(".isEmpty()", ' == ""').replace("&&", "and").replace("null", "None")
        condition = " ".join(condition.split())
        valid = dict(scheme="https", host="relay.example.test", port=443, username="", password="", query=None,
                     fragment=None, body=None, authorization="Bearer fixture-token")
        self.assertTrue(finite_source_predicate(condition, valid))
        for field, value in dict(scheme="http", host="example.test", port=8443, username="user", password="password", query="",
                                 fragment="", body="", authorization="Bearer other").items():
            with self.subTest(field=field):
                self.assertFalse(finite_source_predicate(condition, {**valid, field: value}))
        paths = re.search(r'encodedPath in setOf\(([^\n]+)\)', metadata)[1]
        allowed = re.findall(r'"([^"]+)"', paths)
        self.assertEqual(allowed, ["/v1/capabilities", "/v1/computers"])
        for path in ("/v1/requests", "/v1/computers/", "/v1/computers/other", "/v1/capabilities/", "/", ""):
            with self.subTest(path=path):
                self.assertNotIn(path, allowed)
        self.assertIn('check(count <= 64)', metadata)
        self.assertIn('JSONArray(listOf("inspect", "answer"))', metadata)
        self.assertIn('synchronized(raw) { JSONObject(raw.toString()) }', metadata)
        self.assertIn('put("sessions", JSONArray().put(current))', metadata)
        self.assertIn('return@addInterceptor Response.Builder()', metadata)
        self.assertNotIn("chain.proceed(", source)

    def test_android_broader_proof_and_failure_windows_do_not_rewrite_or_hide_primary_outcome(self):
        source = original_review_fixture()
        proof = source.split("        fun broaderDraftMatches(", 1)[1].split("        fun proveBroaderDraft()", 1)[0]
        for value in ('model.draft(target, question)', 'QuestionPolicies.broaderChoice(question, mapOf("approval" to ids))',
                      'ids == setOf("2")', 'answer.string("text").isEmpty()', 'draft.status == "editing"',
                      'question.run == target.run', 'question.conversation == target.conversation', 'scope?.id == "2"'):
            self.assertIn(value, proof)
        for forbidden in ("model.edit(", "set(model", "tap(", "expected.set("):
            self.assertNotIn(forbidden, proof)
        self.assertIn("record: Boolean = false): Boolean = main {", proof)
        self.assertLess(proof.index("if (!record) checkNotNull(QuestionPolicies.savedDraft(model.drafts, target, question))"),
                        proof.index("val draft = model.draft(target, question)"))
        self.assertIn("if (record) {", proof)
        self.assertEqual(proof.count("phase("), 2)
        self.assertNotIn("await(", proof)
        wrapper = source.split("        fun proveBroaderDraft()", 1)[1].split("        fun proveRejectDraft()", 1)[0]
        self.assertIn('check(broaderDraftMatches(true)) { "Broader Send must preserve its exact selected draft and card" }', wrapper)
        self.assertNotIn("await(", wrapper)
        lifecycle = source.split('            open("background")', 1)[1].split('            boundary("begin", "readonly_stale_identity")', 1)[0]
        binding = 'val lifecycleDisclosure = bindDisclosureScroll("Approve for this session")'
        self.assertLess(lifecycle.index(binding), lifecycle.index('tap("Approve for this session")'))
        restored = lifecycle.split('await("same current card body restored") {', 1)[1].split('\n                }', 1)[0]
        self.assertIn('check(lifecycleSend.current())', restored)
        self.assertIn('check(broaderDraftMatches(false))', restored)
        self.assertIn('disclosureScrollReady(lifecycleDisclosure)', restored)
        for forbidden in ('await(', 'phase(', 'bindDisclosureScroll(', 'performAction(', 'tap('):
            self.assertNotIn(forbidden, restored)
        self.assertNotIn('node("Review synthetic background")', lifecycle)
        self.assertEqual(lifecycle.count('await('), 4)  # Replaces the old node(header) await slot; adds no wait.
        readiness = source.split('    private fun disclosureScrollReady(', 1)[1].split('    private fun screenshot(', 1)[0]
        for required in ('check(scroll.refresh() && scroll.windowId == window && scroll.isScrollable &&',
                         'scroll.packageName?.toString() == targetContext.packageName)',
                         'val active = uiAutomation.rootInActiveWindow ?: return false',
                         'if (!active.refresh() || active.windowId != window ||',
                         'active.packageName?.toString() != targetContext.packageName) return false',
                         'return scroll.isVisibleToUser && !bounds.isEmpty && bounds.left >= 0 && bounds.top >= 0 &&',
                         'bounds.right <= targetContext.resources.displayMetrics.widthPixels && bounds.bottom <= targetContext.resources.displayMetrics.heightPixels'):
            self.assertIn(required, readiness)
        for forbidden in ('await(', 'phase(', 'performAction(', 'bindDisclosureScroll(', 'visible('):
            self.assertNotIn(forbidden, readiness)
        diagnostics = source.split("    private fun evidenceLine(", 1)[1].split("    private fun set(", 1)[0]
        for required in ("windows.size <= 4", "depth <= 16", "++visited <= 128", "++matched <= 12", ".take(160)",
                         'root.packageName?.toString() == targetContext.packageName', 'owned-evidence UNKNOWN stage=$name'):
            self.assertIn(required, diagnostics)
        for forbidden in ("performAction(", "injectInputEvent(", "request.body", "Authorization"):
            self.assertNotIn(forbidden, diagnostics)
        failure = source.split('            result.putString("stream", "Approval choices smoke failed:', 1)[1].split("finish(Activity.RESULT_CANCELED", 1)[0]
        self.assertLess(failure.index("error.stackTraceToString()"), failure.index("$failureEvidence"))

    def test_android_receipt_applied_source_predicate_requires_every_exact_settled_identity(self):
        source = original_review_fixture()
        helper = source.split("        fun receiptApplied(number: Int)", 1)[1].split("        fun open(", 1)[0]
        condition = helper.split("                    val applied =", 1)[1].split("\n                    applied", 1)[0]
        fields = {
            "!model.answerInFlight(draft)": "not in_flight", "!model.detailBusy": "not detail_busy",
            "model.selected?.target": "selected_target", "question?.id": "current_id", "question?.hash": "current_hash",
            "question?.run": "current_run", "question?.conversation": "current_conversation", "draft != null": "draft_present",
            "draft.target": "draft_target", "draft.questionId": "draft_id", "draft.questionHash": "draft_hash",
            "draft.requestId": "draft_request", "draft.answers": "draft_answers", "draft.status": "draft_status",
            "model.pendingQuestionSend": "pending_consent", "model.activityVerified": "verified", "expected.get()": "expected_answer",
            "target.run": "target_run", "target.conversation": "target_conversation",
        }
        for old, new in fields.items():
            condition = condition.replace(old, new)
        condition = " ".join(condition.replace("&&", "and").replace("null", "None").split())
        full_answers = '[{"question_id":"approval","selected_option_ids":["2"],"text":""}]'
        valid = dict(target="target", selected_target="target", questionId="card", current_id="card", questionHash="hash", current_hash="hash",
                     target_run="run", current_run="run", target_conversation="conversation", current_conversation="conversation", draft_present=True,
                     draft_target="target", draft_id="card", draft_hash="hash", requestId="request", draft_request="request", answers=full_answers,
                     draft_answers=full_answers, draft_status="submitted", in_flight=False, pending_consent=None, verified=True, detail_busy=False, expected_answer=None)
        self.assertTrue(finite_source_predicate(condition, valid))
        departures = dict(selected_target="other", current_id="older-card", current_hash="older-hash", current_run="older-run",
                          current_conversation="older-conversation", draft_present=False, draft_target="other", draft_id="older-card",
                          draft_hash="older-hash", draft_request="older-request", draft_answers='[{"selected_option_ids":["1"]}]',
                          draft_status="submitting", in_flight=True, pending_consent="pending", verified=False, detail_busy=True, expected_answer="unconsumed")
        for field, value in departures.items():
            with self.subTest(field=field):
                self.assertFalse(finite_source_predicate(condition, {**valid, field: value}))
        for answer in ('[]', full_answers.replace('"2"', '"1"'), full_answers.replace('"text":""', '"text":"other"'),
                       full_answers.replace('"approval"', '"other"')):
            with self.subTest(answers=answer):
                self.assertFalse(finite_source_predicate(condition, {**valid, "draft_answers": answer}))

    def test_android_receipt_gates_follow_original_counts_without_writes_or_unsent_card_admission(self):
        source = original_review_fixture()
        helper = source.split("        fun receiptApplied(number: Int)", 1)[1].split("        fun open(", 1)[0]
        for required in ('synchronized(calls)', 'check(calls.size == number)', 'JSONObject(calls[number - 1].toString())',
                         'payload.getString("request_id")', 'payload.getString("question_id")', 'payload.getString("expected_question_hash")',
                         'payload.getJSONArray("answers").toString()', 'requestId.isNotBlank() && questionId.isNotBlank() && questionHash.isNotBlank()',
                         'payload.string("expected_run_id") == target.run', 'payload.string("expected_conversation_id") == target.conversation',
                         'model.drafts.singleOrNull { it.target == target && it.questionId == questionId && it.questionHash == questionHash }'):
            self.assertIn(required, helper)
        for forbidden in ('model.draft(', 'model.edit(', 'model.send(', 'set(model', 'expected.set(', 'model.refreshActivity(', 'store.'):
            self.assertNotIn(forbidden, helper)
        self.assertEqual(re.findall(r'^            receiptApplied\((\d)\)$', source, re.M), list("1234"))
        for number, description in enumerate(("once receipt", "broader receipt", "new card reject receipt", "one feedback receipt"), 1):
            wait = source.index('await("' + description + '") { count() == ' + str(number) + ' }')
            gate = source.index('            receiptApplied(' + str(number) + ')')
            self.assertLess(wait, gate)
            self.assertNotIn('open("', source[wait:gate])
        unsent = source.split('            open("replacement")', 1)[1].split('            reviewed(); expect("3")', 1)[0]
        self.assertNotIn('receiptApplied(', unsent)
        self.assertIn('reviewed(); proveBroaderDraft(); tap("Send answer"); node("Confirm this option?")', unsent)
        lifecycle = source.split('            open("background")', 1)[1].split('            boundary("begin", "readonly_stale_identity")', 1)[0]
        self.assertLess(lifecycle.index('proveBroaderDraft()'), lifecycle.index('val lifecycleSend = captureEarlySend()'))
        self.assertEqual(source.count('proveBroaderDraft()'), 5)  # Definition plus the four actual broader Send paths.

    def test_android_failure_is_captured_before_teardown_and_primary_survives_diagnostic_or_cleanup_error(self):
        source = original_review_fixture()
        outer = source.split('    override fun onStart()', 1)[1].split('    private fun <T> main(', 1)[0]
        self.assertNotIn('windowEvidence()', outer)
        self.assertNotIn('screenshot("failure")', outer)
        tail = source.split('        var primaryFailure: Throwable? = null', 1)[1]
        failure, cleanup = tail.rsplit('        } catch (error: Throwable) {', 1)[1].split('        } finally {', 1)
        for earlier, later in [('primaryFailure = error', 'failureEvidence = modelEvidence('),
                               ('failureEvidence = modelEvidence(', 'failureEvidence += "\\n" + windowEvidence()'),
                               ('failureEvidence += "\\n" + windowEvidence()', 'screenshot("failure")'), ('screenshot("failure")', 'throw error')]:
            self.assertLess(failure.index(earlier), failure.index(later))
        self.assertEqual(failure.count('catch (diagnostic: Throwable) { if (diagnostic !== error) error.addSuppressed(diagnostic) }'), 3)
        self.assertNotIn('model.back()', failure)
        self.assertNotIn('activity.finish()', failure)
        self.assertIn('try { main { model.back(); activity.finish() } }', cleanup)
        self.assertIn('val original = primaryFailure', cleanup)
        self.assertIn('if (original == null) throw cleanup else if (cleanup !== original) original.addSuppressed(cleanup)', cleanup)

    def test_android_tap_diagnostics_refresh_and_bind_existing_one_physical_touch_without_new_actions(self):
        source = original_review_fixture()
        tap = source.split('    private fun tap(label: String)', 1)[1].split('    private fun reveal(', 1)[0]
        self.assertLess(tap.index('check(action.refresh())'), tap.index('check(action.isEnabled && action.isClickable)'))
        for required in ('action.windowId', 'action.isVisibleToUser', 'action.isSelected', 'action.isChecked',
                         'phase("touch-geometry:$bounds:x=${bounds.exactCenterX()}:y=${bounds.exactCenterY()}")'):
            self.assertIn(required, tap)
        self.assertLess(tap.index('phase("touch-node:'), tap.index('val down = SystemClock.uptimeMillis()'))
        self.assertLess(tap.index('phase("touch-geometry:'), tap.index('val down = SystemClock.uptimeMillis()'))
        self.assertEqual(tap.count('uiAutomation.injectInputEvent(event, true)'), 1)
        self.assertIn('for (type in listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP))', tap)
        self.assertIn('bounds.exactCenterX(), bounds.exactCenterY(), 0)', tap)
        for forbidden in ('performAction(', 'ACTION_CLICK', 'tap(', 'while ', 'Thread.sleep('):
            self.assertNotIn(forbidden, tap)

    def test_android_reject_proof_source_predicate_requires_existing_current_option_three_without_fabrication(self):
        source = original_review_fixture()
        helper = source.split('        fun proveRejectDraft()', 1)[1].split('        fun expect(', 1)[0]
        self.assertIn('QuestionPolicies.savedDraft(model.drafts, target, it)', helper)
        for forbidden in ('model.draft(', 'model.edit(', 'set(model', 'model.send(', 'tap(', 'expected.set(', 'refreshActivity('):
            self.assertNotIn(forbidden, helper)
        condition = helper.split('                    val proven =', 1)[1].split('\n                    proven', 1)[0]
        fields = {'question != null': 'question_present', 'draft != null': 'draft_present', 'answer != null': 'answer_present',
                  'selected != null': 'selection_present', 'offered.string("question_id")': 'offered_id',
                  'offered.string("question_hash")': 'offered_hash', 'question.id': 'current_id', 'question.hash': 'current_hash',
                  'question.run': 'current_run', 'question.conversation': 'current_conversation', 'model.selected?.target': 'selected_target',
                  'draft.target': 'draft_target', 'draft.questionId': 'draft_id', 'draft.questionHash': 'draft_hash',
                  'draft.status': 'status', 'draft.requestId.isEmpty()': 'unsubmitted', '!model.answerInFlight(draft)': 'not in_flight',
                  'answer.opt("question_id")': 'answer_id', 'selected.length()': 'option_count', 'selected.opt(0)': 'option',
                  'answer.opt("text")': 'actual_text', 'model.pendingQuestionSend': 'pending_consent',
                  'model.activityVerified': 'verified', 'question.canAnswer': 'answerable', 'QuestionPolicies.nativeChoices(question)': 'native_choices',
                  'target.run': 'target_run', 'target.conversation': 'target_conversation'}
        for old, new in fields.items():
            condition = condition.replace(old, new)
        condition = ' '.join(condition.replace('&&', 'and').replace('null', 'None').split())
        valid = dict(question_present=True, draft_present=True, answer_present=True, selection_present=True,
                     offered_id='replacement-new', current_id='replacement-new', offered_hash='new-hash', current_hash='new-hash',
                     current_run='run', target_run='run', current_conversation='conversation', target_conversation='conversation',
                     target='target', selected_target='target', draft_target='target', draft_id='replacement-new', draft_hash='new-hash',
                     status='editing', unsubmitted=True, in_flight=False, answer_id='approval', option_count=1, option='3', actual_text='',
                     pending_consent=None, verified=True, answerable=True, native_choices=True)
        self.assertTrue(finite_source_predicate(condition, valid))
        departures = dict(question_present=False, draft_present=False, answer_present=False, selection_present=False,
                          current_id='old', current_hash='old', current_run='old', current_conversation='old', selected_target='other',
                          draft_target='other', draft_id='old', draft_hash='old', status='submitted', unsubmitted=False, in_flight=True,
                          answer_id='other', option_count=2, option='2', actual_text='other', pending_consent='pending', verified=False,
                          answerable=False, native_choices=False)
        for field, value in departures.items():
            with self.subTest(field=field):
                self.assertFalse(finite_source_predicate(condition, {**valid, field: value}))
        for field, value in (('option', 3), ('option', None), ('actual_text', None), ('actual_text', False), ('answer_id', None)):
            with self.subTest(field=field, type=type(value).__name__):
                self.assertFalse(finite_source_predicate(condition, {**valid, field: value}))
        replacement = source.split('            open("replacement-new")', 1)[1].split('            cases += "card_replacement"', 1)[0]
        self.assertEqual(replacement.count('proveRejectDraft()'), 2)
        self.assertLess(replacement.index('tap("Reject")'), replacement.index('proveRejectDraft()'))
        self.assertLess(replacement.index('proveRejectDraft()'), replacement.index('earlyReview("replacement")'))
        self.assertLess(replacement.index('earlyReview("replacement")'), replacement.rindex('proveRejectDraft()'))
        self.assertLess(replacement.rindex('proveRejectDraft()'), replacement.index('reviewed(); expect("3")'))
        self.assertIn('elapsed >= 0 && elapsed < 3_000', source)

    def test_android_staged_diagnostic_fault_retains_active_partial_when_window_query_throws(self):
        source = original_review_fixture()
        record, stage, order = android_diagnostic_memory(source)
        lines, calls = [], []
        def active():
            calls.append('active')
            record(lines, 'owned-root active bounds=fixture')
            record(lines, 'owned-label literal=Review synthetic replacement-new')
        def window_query():
            calls.append('windows')
            raise LookupError('memory-only windows query fault')
        for name, callback in zip(order, (active, window_query), strict=True):
            stage(lines, name, callback)
        self.assertEqual(calls, ['active', 'windows'])
        self.assertEqual(lines, ['owned-root active bounds=fixture', 'owned-label literal=Review synthetic replacement-new',
                                 'owned-evidence UNKNOWN stage=window-list type=LookupError'])
        def partial_root():
            record(lines, 'owned-label literal=Reject')
            raise ValueError('memory-only traversal fault')
        stage(lines, 'active-root', partial_root)
        self.assertEqual(lines[-2:], ['owned-label literal=Reject', 'owned-evidence UNKNOWN stage=active-root type=ValueError'])
        bounded = []
        for _ in range(100):
            record(bounded, 'x' * 1000)
        self.assertEqual(len(bounded), 64)
        self.assertTrue(all(len(line) <= 160 for line in bounded))
        self.assertEqual(bounded[-1], 'owned-evidence UNKNOWN line-limit')

    def test_android_staged_diagnostics_and_cleanup_preserve_the_identical_primary_in_memory(self):
        source = original_review_fixture()
        record, stage, order = android_diagnostic_memory(source)
        failure = source.split('        var primaryFailure: Throwable? = null', 1)[1].rsplit('        } catch (error: Throwable) {', 1)[1]
        capture, cleanup = failure.split('        } finally {', 1)
        callbacks = re.findall(r'^            try \{ (?:failureEvidence [+=]+ (?:"\\n" \+ )?)?(modelEvidence|windowEvidence|screenshot)\(', capture, re.M)
        self.assertEqual(callbacks, ['modelEvidence', 'windowEvidence', 'screenshot'])
        self.assertLess(capture.index('primaryFailure = error'), capture.index('modelEvidence('))
        self.assertIn('throw error', capture)
        self.assertIn('if (diagnostic !== error) error.addSuppressed(diagnostic)', capture)
        self.assertIn('if (original == null) throw cleanup else if (cleanup !== original) original.addSuppressed(cleanup)', cleanup)
        primary, window_error, cleanup_error = RuntimeError('primary'), LookupError('windows'), OSError('cleanup')
        lines, suppressed, events = [], [], []
        def model():
            events.append('model')
            record(lines, 'owned-model option=3 complete=true')
        def windows():
            events.append('window')
            stage(lines, order[0], lambda: record(lines, 'owned-root active'))
            def query():
                raise window_error
            stage(lines, order[1], query)
        def screenshot():
            events.append('screenshot')
            raise primary  # Same-object diagnostic must not self-suppress or replace it.
        actions = dict(modelEvidence=model, windowEvidence=windows, screenshot=screenshot)
        caught = None
        try:
            try:
                raise primary
            except BaseException as error:
                original = error
                for callback in callbacks:  # Exact source callback order, memory-only replacements.
                    try:
                        actions[callback]()
                    except BaseException as diagnostic:
                        if diagnostic is not error:
                            suppressed.append(diagnostic)
                raise
            finally:
                events.append('cleanup')
                try:
                    raise cleanup_error
                except BaseException as error:
                    if original is None:
                        raise
                    if error is not original:
                        suppressed.append(error)
        except BaseException as error:
            caught = error
        self.assertIs(caught, primary)
        self.assertEqual(suppressed, [cleanup_error])
        self.assertEqual(events, ['model', 'window', 'screenshot', 'cleanup'])
        self.assertEqual(lines, ['owned-model option=3 complete=true', 'owned-root active',
                                 'owned-evidence UNKNOWN stage=window-list type=LookupError'])

    def test_android_failure_labels_model_and_tree_diagnostics_are_fixed_owned_and_readonly(self):
        source = original_review_fixture()
        diagnostics = source.split('    private fun evidenceLine(', 1)[1].split('    private fun set(', 1)[0]
        for required in ('root.refresh() && root.packageName?.toString() == targetContext.packageName',
                         'node.packageName?.toString() == targetContext.packageName', 'depth <= 16', '++visited <= 128', '++matched <= 12',
                         '++scrolls <= 4', 'windows.size <= 4', 'kind=$kind', 'root.getBoundsInScreen(rootBounds)',
                         'printf fixture # *literal* [not a link](https://example.test)', 'Scroll for options ↓',
                         'Choose one of the agent\'s options.', 'Review the request, then choose one of the agent\'s options ($it).',
                         '(1..3).map', '"Review synthetic $it"', 'QuestionPolicies.savedDraft(model.drafts, target, it)',
                         'activity.hasWindowFocus()', 'activity.lifecycle.currentState.name', 'model.activityVerified', 'model.detailBusy',
                         'model.pendingQuestionSend != null', 'QuestionPolicies.complete(', 'QuestionPolicies.nativeChoices'):
            if required == 'QuestionPolicies.nativeChoices':
                self.assertIn('QuestionPolicies::nativeChoices', diagnostics)
            else:
                self.assertIn(required, diagnostics)
        for forbidden in ('performAction(', 'injectInputEvent(', 'model.draft(', 'model.edit(', 'model.send(', 'set(model', 'expected.set(',
                          'refreshActivity(', 'setServiceInfo(', 'FLAG_RETRIEVE_INTERACTIVE_WINDOWS', 'Thread.sleep(', 'SystemClock.sleep('):
            self.assertNotIn(forbidden, diagnostics)

    def test_ios_reverse_ack_source_token_guard_nonce_order_and_teardown_refuse_unknown(self):
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        receiver = source.split("private final class ReviewAcknowledgement", 1)[1].split("/// Native SwiftUI", 1)[0]
        guard = re.search(r'guard ([^\n]+) else \{ failure = "stale-or-repeated-reverse-ack"', receiver)[1]
        condition = guard.replace("!consumed", "not consumed").replace(",", " and")
        self.assertTrue(finite_source_predicate(condition, dict(consumed=False, token=17, received=17)))
        for values in (dict(consumed=True, token=17, received=17), dict(consumed=False, token=None, received=17),
                       dict(consumed=False, token=17, received=18)):
            with self.subTest(values=values):
                self.assertFalse(finite_source_predicate(condition, values))
        self.assertLess(receiver.index("token = nil //"), receiver.index("guard notify_cancel(received) == 0"))
        self.assertLess(receiver.index("guard notify_cancel(received) == 0"), receiver.index("receivedAt ="))
        self.assertNotIn("@unchecked", receiver)
        begin = source.split("    private func beginReview(", 1)[1].split("    private func ack(", 1)[0]
        self.assertIn("addTeardownBlock { () async in await receiver.close() }", begin)
        for earlier, later in [('addTeardownBlock', 'receiver.register(reverse)'), ('receiver.register(reverse)', 'let began ='),
                               ('let began =', 'notify_post($0)'), ('notify_post($0)', 'receiver.wait()')]:
            self.assertLess(begin.index(earlier), begin.index(later))
        self.assertNotIn('staticTexts[', begin.split("let began =", 1)[1])
        for code in ("c", "f"):
            name = "zui.ack." + ".".join(["a" * 36, "b" * 36, "c" * 36, code])
            self.assertLessEqual(len(name), 128)
        host = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalUIFixtureApp.swift").read_text()
        self.assertIn('selectedAcknowledgement != selectedLaunch && selectedAcknowledgement != generation.lowercased()', host)
        self.assertLess(host.index('status = "ack|'), host.index('guard reverse.withCString'))
        self.assertIn('case .replacement: [.replace]; case .countdown: [.beginCountdownReview]', host)
        countdown = source.split("    func testRealCountdownAndOneTimeChoice()", 1)[1].split("    func testBroader", 1)[0]
        for earlier, later in [('launchFixture(.countdown)', 'choice("Approve once").tap()'), ('choice("Approve once").tap()', 'beginReview("begin-countdown-review")'),
                               ('beginReview("begin-countdown-review")', 'XCTAssertFalse(send.isEnabled); count(0)'),
                               ('XCTAssertLessThan(earlyElapsed, 3', 'ack("begin-countdown-review", serial: 0)')]:
            self.assertLess(countdown.index(earlier), countdown.index(later))
        self.assertNotIn('press(.home)', countdown)

    def test_ios_adaptive_cancel_requires_owned_popover_and_passive_outside_anchor(self):
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        cancel = source.split("    private func cancelBroaderConfirmation()", 1)[1].split("    func testRealCountdown", 1)[0]
        for required in ('if app.buttons["Cancel"].exists', 'XCTAssertEqual(app.popovers.count, 1',
                         'popover.buttons["Confirm"].exists && popover.staticTexts[disclosure].exists',
                         'XCTAssertTrue(anchor.exists)', 'XCTAssertEqual(anchor.identifier, "fixture-dismiss-anchor")',
                         'XCTAssertEqual(anchor.label, "Synthetic UI only — no native delivery")', '["regular", "compact"].contains(trait)',
                         'candidate.staticTexts["fixture-dismiss-anchor"].exists && candidate.popovers.count == 1',
                         'geometryCheck(windows.count == 1', 'positiveFinite(actual) && actual.contains(popup)',
                         'frame.intersection(window).intersection(canvas)', '!region.intersects(popup)',
                         'let coordinate = anchor.coordinate(withNormalizedOffset: offset)',
                         'let point = coordinate.screenPoint', 'XCTAssertEqual(app.state, .runningForeground)',
                         'geometryCheck(pointInside,', 'coordinate.tap()',
                         'XCTWaiter.wait(for: [retired], timeout: 2)'):
            self.assertIn(required, cancel)
        self.assertLess(cancel.index('XCTAssertEqual(app.popovers.count, 1'), cancel.index('anchor.coordinate('))
        self.assertLess(cancel.index('let point = coordinate.screenPoint'), cancel.index('geometryCheck(pointInside,'))
        self.assertLess(cancel.index('geometryCheck(pointInside,'), cancel.index('coordinate.tap()'))
        self.assertEqual(cancel.count('coordinate.tap()'), 1)
        for forbidden in ('notify_post(', 'while ', 'try?', 'coordinate(withOffset:', 'broader =', 'available =', '.isHittable',
                          '!popup.intersects(frame)', 'actual.contains(frame)'):
            self.assertNotIn(forbidden, cancel)
        host = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalUIFixtureApp.swift").read_text()
        anchor = host.split('Text("Synthetic UI only — no native delivery")', 1)[1].split('Text("Callbacks:', 1)[0]
        self.assertIn('.accessibilityIdentifier("fixture-dismiss-anchor")', anchor)
        self.assertNotIn("Button(", anchor)
        self.assertNotIn("onTapGesture", anchor)

    def test_ios_outside_coordinate_source_point_refuses_edges_other_windows_and_popover(self):
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        condition = source.split('            let pointInside = ', 1)[1].split('\n            guard geometryCheck(pointInside,', 1)[0]
        condition = condition.replace('!popup.contains(point)', 'not popup_contains').replace('&&', ' and ')
        for value in ('point.x', 'point.y', 'frame.minX', 'frame.maxX', 'frame.minY', 'frame.maxY',
                      'window.minX', 'window.maxX', 'window.minY', 'window.maxY',
                      'canvas.minX', 'canvas.maxX', 'canvas.minY', 'canvas.maxY',
                      'region.minX', 'region.maxX', 'region.minY', 'region.maxY'):
            condition = condition.replace(value, value.replace('.', '_'))
        condition = '(' + condition + ')'
        good = dict(point_x=50, point_y=50, frame_minX=40, frame_maxX=60, frame_minY=40, frame_maxY=60,
                    window_minX=0, window_maxX=100, window_minY=0, window_maxY=100,
                    canvas_minX=0, canvas_maxX=100, canvas_minY=0, canvas_maxY=100,
                    region_minX=45, region_maxX=55, region_minY=45, region_maxY=55, popup_contains=False)
        self.assertTrue(finite_source_predicate(condition, good))
        departures = [dict(popup_contains=True), dict(point_x=float('nan')), dict(point_y=float('inf'))]
        for axis in ('X', 'Y'):
            for owner in ('frame', 'window', 'canvas', 'region'):
                departures += [{owner + '_min' + axis: 50}, {owner + '_max' + axis: 50}]
        for change in departures:
            with self.subTest(change=change):
                self.assertFalse(finite_source_predicate(condition, {**good, **change}))

    def test_ios_outside_geometry_uses_finite_clipped_unoccluded_source_regions(self):
        """Finite arithmetic controls for the source; no CGRect, Swift compiler or native gesture claim."""
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        helper = source.split('    private func positiveFinite(', 1)[1].split('    private func cancelBroader', 1)[0]
        fields = re.search(r'\[([^\n]+)\]\.allSatisfy \{ \$0.isFinite \}', helper)[1].split(', ')
        self.assertEqual(fields, ['rect.minX', 'rect.minY', 'rect.maxX', 'rect.maxY', 'rect.width', 'rect.height'])
        positive = helper.split(' && ', 1)[1].split('\n', 1)[0].replace('rect.', '').replace('&&', 'and')
        def qualifies(rect):
            return all(math.isfinite(rect[field.split('.')[1]]) for field in fields) and finite_source_predicate(positive, rect)
        valid = dict(minX=0, minY=0, maxX=100, maxY=100, width=100, height=100)
        self.assertTrue(qualifies(valid))
        for field in valid:
            for invalid in (float('nan'), float('inf'), -float('inf')):
                with self.subTest(field=field, invalid=invalid):
                    self.assertFalse(qualifies({**valid, field: invalid}))
        for field in ('width', 'height'):
            for invalid in (0, -1):
                self.assertFalse(qualifies({**valid, field: invalid}))
        bounds = source.split('let bounds: [(CGFloat, CGFloat, CGFloat, CGFloat)] = ', 1)[1].split('\n            guard let selected', 1)[0]
        bounds = re.sub(r'(visible|popup)\.(minX|minY|maxX|maxY)', r'\1_\2', bounds)
        tree = ast.parse(bounds, mode='eval')
        allowed = (ast.Expression, ast.List, ast.Tuple, ast.Name, ast.Load, ast.Call)
        self.assertTrue(all(isinstance(node, allowed) for node in ast.walk(tree)))
        self.assertEqual({node.func.id for node in ast.walk(tree) if isinstance(node, ast.Call)}, {'min', 'max'})
        selector = re.search(r'bounds.first\(where: \{ ([^\n]+) \}\)', source)[1]
        for index, name in enumerate(('minX', 'minY', 'maxX', 'maxY')):
            selector = selector.replace('$0.' + str(index), name)
        selector = selector.replace('&&', 'and')
        def region(visible, popup):
            values = {owner + '_' + field: value for owner, rect in (('visible', visible), ('popup', popup))
                      for field, value in zip(('minX', 'minY', 'maxX', 'maxY'), rect)}
            self.assertLessEqual({node.id for node in ast.walk(tree) if isinstance(node, ast.Name)}, values.keys() | {'min', 'max'})
            candidates = eval(compile(tree, '<finite-source-regions>', 'eval'), {'__builtins__': {}, 'min': min, 'max': max}, values)
            self.assertEqual(len(candidates), 4)  # One geometric choice before the single gesture, never tap retries.
            return next((row for row in candidates if finite_source_predicate(selector, dict(zip(('minX', 'minY', 'maxX', 'maxY'), row)))), None)
        visible = (10, 10, 90, 90)
        for popup in ((20, 20, 80, 80), (0, 0, 100, 50), (50, 0, 100, 100), (0, 0, 50, 100),
                      (0, 90, 100, 100), (90, 0, 100, 100), (100, 100, 120, 120)):
            with self.subTest(popup=popup):
                selected = region(visible, popup)
                self.assertIsNotNone(selected)
                left, top, right, bottom = selected
                x, y = (left + right) / 2, (top + bottom) / 2
                self.assertTrue(visible[0] < x < visible[2] and visible[1] < y < visible[3])
                self.assertFalse(popup[0] <= x <= popup[2] and popup[1] <= y <= popup[3])
        for popup in (visible, (0, 0, 100, 100)):
            self.assertIsNone(region(visible, popup))
        cancel = source.split('    private func cancelBroaderConfirmation()', 1)[1].split('    func testRealCountdown', 1)[0]
        self.assertLess(cancel.index('phase("cancel-geometry-popup-'), cancel.index('geometryCheck(positiveFinite(popup)'))
        self.assertLess(cancel.index('phase("cancel-geometry-window-'), cancel.index('positiveFinite(actual)'))
        self.assertLess(cancel.index('phase("cancel-geometry-visible-anchor-'), cancel.index('geometryCheck(positiveFinite(visible)'))
        diagnostic = source.split('    private func geometryCheck(', 1)[1].split('    private func positiveFinite(', 1)[0]
        self.assertLess(diagnostic.index('phase('), diagnostic.index('XCTAssertTrue(condition, criterion)'))

    def test_ios_unavailable_event_follows_original_dialog_and_is_single_mode_bound(self):
        host = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalUIFixtureApp.swift").read_text()
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        controls = host.split('final class FixtureControls {', 1)[1].split('// This file belongs only', 1)[0]
        self.assertIn('case becomeUnavailable = "become-unavailable"', controls)
        self.assertIn('case .unavailable: [.becomeUnavailable]', controls)
        self.assertIn('case .becomeUnavailable: available = false', controls)
        self.assertNotIn('Unavailable in 2 seconds', host + source)
        self.assertNotIn('Task.sleep', controls)
        self.assertIn('case .replace, .becomeUnavailable: nil', controls)
        actions = controls.split('    private var actions: [Action] {', 1)[1].split('    private func name(', 1)[0]
        mode_actions = re.findall(r'case \.(\w+): \[([^\]]*)\]', actions)
        self.assertEqual(len(mode_actions), 5)
        self.assertEqual(dict(mode_actions), dict(normal='', feedback='.beginFeedbackReview', replacement='.replace',
                                                  countdown='.beginCountdownReview', unavailable='.becomeUnavailable'))
        raw = re.search(r'guard (!closed, failure.isEmpty, actions.contains\(action\), !consumed.contains\(action\), tokens\[action\] == token) else', controls)[1]
        predicate = raw.replace('!closed', 'not closed').replace('failure.isEmpty', 'failure_empty').replace('actions.contains(action)', 'mode_admits').replace('!consumed.contains(action)', 'not consumed').replace('tokens[action]', 'owned_token').replace(',', ' and')
        for mode in ('normal', 'feedback', 'replacement', 'countdown', 'unavailable'):
            valid = dict(closed=False, failure_empty=True, mode_admits='.becomeUnavailable' in dict(mode_actions)[mode], consumed=False, owned_token=17, token=17)
            self.assertEqual(finite_source_predicate(predicate, valid), mode == 'unavailable')
        valid = dict(closed=False, failure_empty=True, mode_admits=True, consumed=False, owned_token=17, token=17)
        for changed in (dict(closed=True), dict(failure_empty=False), dict(consumed=True), dict(owned_token=None), dict(token=18)):
            self.assertFalse(finite_source_predicate(predicate, {**valid, **changed}))
        case = source.split('    func testUnavailableRetiresAnOpenConfirmation()', 1)[1].split('    func testFeedback', 1)[0]
        for earlier, later in [('launchFixture(.unavailable)', 'send.tap()'), ('send.tap()', 'waitForExistence(timeout: 1)'),
                               ('waitForExistence(timeout: 1)', 'count(0)'), ('count(0)', 'app.staticTexts['),
                               ('app.staticTexts[', 'post("become-unavailable")'), ('post("become-unavailable")', 'XCTWaiter.wait(for: [retired]'),
                               ('XCTWaiter.wait(for: [retired]', 'ack("become-unavailable", serial: 0)'),
                               ('ack("become-unavailable", serial: 0)', 'count(0); XCTAssertFalse(send.isEnabled)')]:
            self.assertLess(case.index(earlier), case.index(later))
        self.assertIn('changes the agent\'s permissions beyond this request.\\n\\nExact command for this session only', case)
        self.assertIn('NSPredicate(format: "exists == false")', case)
        self.assertIn('XCTWaiter.wait(for: [retired], timeout: 4)', case)
        self.assertEqual(case.count('post("become-unavailable")'), 1)

    def test_ios_feedback_refocus_preserves_real_early_return_text_and_final_oracle(self):
        source = (ROOT / "mobile/ios/ApprovalUIFixture/ApprovalChoicesUITests.swift").read_text()
        feedback = source.split('    func testFeedbackKeyboardCannotBypassReview()', 1)[1].split('    func testBackground', 1)[0]
        early, late = feedback.split('        count(0)\n', 1)
        self.assertIn('field.typeText("d\\n")', early)
        self.assertIn('XCTAssertLessThan(earlyElapsed, 3', early)
        self.assertIn('ack("begin-feedback-review", serial: 0)', early)
        self.assertEqual(early.count('field.tap()'), 1)
        for earlier, later in [('XCTAssertEqual(field.value as? String, "Use a safer command")', 'field.tap()'),
                               ('field.tap()', 'count(0)'), ('count(0)', 'reviewed(); field.typeText("\\n")')]:
            self.assertLess(late.index(earlier), late.index(later))
        self.assertEqual(late.count('field.tap()'), 1)
        self.assertEqual(late.count('XCTAssertEqual(field.value as? String, "Use a safer command")'), 2)
        self.assertEqual(feedback.count('field.typeText('), 2)
        self.assertIn('XCTAssertEqual(answer, "fixture-hash-0|[', late)
        self.assertNotIn('typeText("Use a safer command', feedback)
        self.assertNotIn('sleep(', feedback)


    def test_android_stale_case_delivers_inspection_before_one_negative_physical_attempt(self):
        source = original_review_fixture()
        role_seams = re.compile(r'^([ ]*)// BEGIN STALE_COMPOSE_ROLE ([a-z-]+)\n[\s\S]*?^\1// END STALE_COMPOSE_ROLE \2\n', re.M)
        self.assertEqual([match[2] for match in role_seams.finditer(source)], ['definition', 'admission'])
        source = role_seams.sub('', source)
        case = source.split('            boundary("begin", "readonly_stale_identity")', 1)[1].split('            boundary("complete", "readonly_stale_identity")', 1)[0]
        mutation = 'synchronized(raw) { raw.put("run_id", "replacement-run") }'
        refresh = 'main { model.refreshActivity() }'
        observed = 'await("delivered replacement-run inspection") { staleInspectionCurrent() }'
        gesture = 'for (type in listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP))'
        self.assertLess(case.index('tap("Approve once"); reviewed()'), case.index(mutation))
        self.assertLess(case.index(mutation), case.index(refresh))
        self.assertLess(case.index(refresh), case.index(observed))
        self.assertLess(case.index(observed), case.index('val root = checkNotNull(uiAutomation.rootInActiveWindow)'))
        self.assertLess(case.index('val action = staleDisabledAction(label, "Send answer")'), case.index(gesture))
        self.assertLess(case.index(gesture), case.index('requireHealthyFixture()'))
        self.assertLess(case.index('requireHealthyFixture()'), case.index('check(staleInspectionCurrent())'))
        self.assertEqual(case.count(refresh), 1)
        self.assertEqual(case.count(gesture), 1)
        self.assertEqual(case.count('uiAutomation.injectInputEvent(event, true)'), 1)
        self.assertIn('if (type == MotionEvent.ACTION_DOWN) SystemClock.sleep(100)', case)
        self.assertIn('check(!postStagesUnknown && answerEntries == 4 && count() == 4 && expected.get() == null)', case)
        self.assertIn('check(count() == 4 && expected.get() == null && postStages.get(2) == answerEntries)', case)
        self.assertIn('stale-send status=observed inspectedRun=replacement-run cardRun=fixture-run', case)
        self.assertIn('answerEntryBefore=$answerEntries answerEntryAfter=${postStages.get(2)} receipts=4 draft=unchanged composer=unchanged', case)
        self.assertIn('$staleSendEvidence\\n', source)
        after_mutation = case.split(mutation, 1)[1]
        for forbidden in ('tap(', 'reviewed()', 'expect(', 'expected.set(', 'expected.compareAndSet(',
                          'model.edit(', 'model.sendQuestion(', 'model.draft(', 'performAction(', 'ACTION_CLICK'):
            self.assertNotIn(forbidden, after_mutation)

    def test_android_stale_inspection_source_predicate_refuses_every_unknown_or_changed_identity(self):
        source = original_review_fixture()
        case = source.split('            boundary("begin", "readonly_stale_identity")', 1)[1].split('            boundary("complete", "readonly_stale_identity")', 1)[0]
        condition = case.split('                val proven =', 1)[1].split('\n                proven', 1)[0]
        fields = {'activity.hasWindowFocus()': 'focused', 'model.activityVerified': 'verified', 'model.detailBusy': 'busy',
                  'model.selected?.target': 'selected_target', 'replacementTarget.run': 'replacement_run', 'replacementTarget': 'replacement_target',
                  'model.activity?.string("run_id")': 'inspected_run', 'model.activity?.string("conversation_id")': 'inspected_conversation',
                  'question != null': 'question_present', 'question == staleQuestion': 'card_unchanged', 'question.id': 'card_id',
                  'question.hash': 'card_hash', 'question.run': 'card_run', 'question.conversation': 'card_conversation',
                  'target.run': 'old_run', 'target.conversation': 'old_conversation', 'draft == staleDraft': 'draft_unchanged',
                  'composer == staleComposer': 'composer_unchanged', 'model.answerInFlight(staleDraft)': 'in_flight',
                  'model.pendingQuestionSend': 'pending_consent',
                  'QuestionSendPolicies.bound(question, replacementTarget)': 'binding_matches',
                  'QuestionSendPolicies.available(question, replacementTarget, staleDraft.answers)': 'complete_answer_allowed'}
        for old, new in sorted(fields.items(), key=lambda item: -len(item[0])):
            condition = condition.replace(old, new)
        condition = ' '.join(re.sub(r'!(?!=)', 'not ', condition).replace('&&', 'and').replace('null', 'None').split())
        valid = dict(focused=True, verified=True, busy=False, selected_target='replacement', replacement_target='replacement',
                     inspected_run='replacement-run', replacement_run='replacement-run', inspected_conversation='conversation',
                     old_run='fixture-run', old_conversation='conversation', question_present=True, card_unchanged=True,
                     card_id='stale-identity', card_hash='hash-stale-identity', card_run='fixture-run', card_conversation='conversation',
                     draft_unchanged=True, composer_unchanged=True, in_flight=False, pending_consent=None,
                     binding_matches=False, complete_answer_allowed=False)
        self.assertTrue(finite_source_predicate(condition, valid))
        departures = dict(focused=False, verified=False, busy=True, selected_target='old', inspected_run=None,
                          inspected_conversation=None, question_present=False, card_unchanged=False, card_id='old', card_hash='old',
                          card_run='replacement-run', card_conversation='other', draft_unchanged=False, composer_unchanged=False,
                          in_flight=True, pending_consent='consent', binding_matches=True, complete_answer_allowed=True)
        for field, value in departures.items():
            with self.subTest(field=field):
                self.assertFalse(finite_source_predicate(condition, {**valid, field: value}))
        self.assertIn('} && !postStagesUnknown && postStages.get(2) == answerEntries && count() == 4 && expected.get() == null', case)

    def test_android_stale_negative_gesture_requires_fresh_owned_disabled_label_and_unchanged_bounds(self):
        source = original_review_fixture()
        role_seams = re.compile(r'^([ ]*)// BEGIN STALE_COMPOSE_ROLE ([a-z-]+)\n[\s\S]*?^\1// END STALE_COMPOSE_ROLE \2\n', re.M)
        self.assertEqual([match[2] for match in role_seams.finditer(source)], ['definition', 'admission'])
        source = role_seams.sub('', source)
        case = source.split('            boundary("begin", "readonly_stale_identity")', 1)[1].split('            boundary("complete", "readonly_stale_identity")', 1)[0]
        before_touch = case.split('            val ownedWindow = main {', 1)[1].split('            val down = SystemClock.uptimeMillis()', 1)[0]
        for required in ('activity.window.decorView.createAccessibilityNodeInfo()', 'check(activity.hasWindowFocus())',
                         'root.windowId == ownedWindow && root.packageName?.toString() == targetContext.packageName',
                         'checkNotNull(find(root) { exactVisible(it, "Send answer") })',
                         'label.refresh() && label.windowId == ownedWindow && exactVisible(label, "Send answer")',
                         'staleDisabledAction(label, "Send answer") == action', 'bounds.intersect(labelBounds) && !bounds.isEmpty',
                         'Rect().also(label::getBoundsInScreen) == labelBounds', 'Rect().also(action::getBoundsInScreen) == actionBounds',
                         'currentRoot.refresh() && currentRoot.windowId == ownedWindow', 'staleInspectionCurrent()'):
            self.assertIn(required, before_touch)
        self.assertIn('check(earlySend == null && staleInspectionCurrent())', case)
        self.assertIn('QuestionPolicies.complete(question.prompts, mapOf("approval" to setOf("1")), emptyMap(), QuestionPolicies.formReplyText(question))', case)
        self.assertIn('check(QuestionSendPolicies.bound(question, target) && QuestionSendPolicies.available(question, target, draft.answers))', case)
        self.assertIn('label.refresh() && label.windowId == ownedWindow && label.packageName?.toString() == targetContext.packageName && exactVisible(label, text)', case)
        self.assertIn('check(!question.prompts.single().multi)', case)
        self.assertIn('val choiceAction = staleDisabledAction(choice, "Approve once")', case)
        self.assertIn('check(staleDisabledAction(choice, "Approve once") == choiceAction && Rect().also(choiceAction::getBoundsInScreen) == choiceBounds)', case)
        self.assertIn('touchTrace?.begin("Send answer", ownedWindow, down, bounds.exactCenterX(), bounds.exactCenterY())', case)
        self.assertIn('touchTrace?.end(token)', case)
        self.assertIn('finally { event.recycle() }', case)
        for forbidden in ('earlySend =', 'node("Send answer")', 'button("Send answer")', 'ACTION_CLICK', 'setReviewDelayForTests'):
            self.assertNotIn(forbidden, before_touch)
        normal = source.split('    private fun tap(label: String)', 1)[1].split('    private fun reveal(', 1)[0]
        self.assertIn('check(action.isEnabled && action.isClickable)', normal)
        self.assertEqual(normal.count('uiAutomation.injectInputEvent(event, true)'), 1)

    def test_android_question_binding_is_shared_by_complete_answer_admission_and_all_native_card_actions(self):
        policies = (ROOT / "mobile/android/app/src/main/java/app/zerus/mobile/QuestionSendConfirmation.kt").read_text()
        condition = policies.split('    fun bound(question: Question, target: Target) =', 1)[1].split('\n    // Kimi', 1)[0]
        fields = {'target.run.isNotBlank()': 'nonblank_run', 'question.run': 'card_run', 'target.run': 'target_run',
                  'question.conversation': 'card_conversation', 'target.conversation': 'target_conversation'}
        for old, new in sorted(fields.items(), key=lambda item: -len(item[0])):
            condition = condition.replace(old, new)
        condition = ' '.join(condition.replace('&&', 'and').split())
        for conversation in ('fixture-conversation', ''):
            valid = dict(nonblank_run=True, card_run='fixture-run', target_run='fixture-run',
                         card_conversation=conversation, target_conversation=conversation)
            self.assertTrue(finite_source_predicate(condition, valid))
            for field, value in dict(nonblank_run=False, card_run='other-run', card_conversation='other-conversation').items():
                with self.subTest(conversation=conversation, field=field):
                    self.assertFalse(finite_source_predicate(condition, {**valid, field: value}))
        available = policies.split('    fun available(', 1)[1].split('\n}\n', 1)[0]
        self.assertLess(available.index('if (!bound(question, target)) return false'), available.index('QuestionPolicies.submitted('))
        self.assertLess(available.index('if (!bound(question, target)) return false'), available.index('JSONArray(answers)'))
        model = (ROOT / "mobile/android/app/src/main/java/app/zerus/mobile/ZerusViewModel.kt").read_text()
        send = model.split('    private fun sendQuestion(', 1)[1].split('    fun sendingBlocked(', 1)[0]
        self.assertLess(send.index('QuestionSendPolicies.available(native, target, frozenAnswers)'), send.index('QuestionSendPolicies.capture('))
        self.assertLess(send.index('intent.permitsPrepared('), send.index('api.submit('))
        ui = (ROOT / "mobile/android/app/src/main/java/app/zerus/mobile/QuestionUi.kt").read_text()
        card = ui.split('@Composable private fun NativeQuestionCard(', 1)[1]
        self.assertIn('frameAvailable: Boolean', card)
        self.assertIn('val bound = QuestionSendPolicies.bound(question, target)', card)
        self.assertIn('val available = frameAvailable && bound', card)
        self.assertIn('val editing = bound && draft.status == "editing" && native == null && model.pendingQuestionSend == null', card)
        for required in ('question.canAnswer && editing && complete && available', 'question.canAnswer && editing && available',
                         'reviewEligible', 'enabled = editing && available', 'QuestionPolicies.inputEnabled(question, editing)',
                         'readOnly = !QuestionPolicies.inputEnabled(question, editing)'):
            self.assertIn(required, card)
        self.assertIn('if (question != null) NativeQuestionCard(model, target, question, available', ui)

    def test_android_stale_choice_resolves_disabled_action_ancestor_independently_of_label_enabled_state(self):
        source = original_review_fixture()
        walker = source.split('            fun staleDisabledAction(label: AccessibilityNodeInfo, text: String): AccessibilityNodeInfo {', 1)[1].split('            val choice = checkNotNull(', 1)[0]
        roles = dict(re.findall(r'"([^"]+)" -> "([^"]+)"', walker))
        self.assertEqual(roles, {'Approve once': 'android.widget.RadioButton', 'Send answer': 'android.widget.Button'})
        self.assertIn('else -> error("Unknown stale action label")', walker)
        self.assertIn('exactVisible(label, text)', walker)
        self.assertIn('candidate = checkNotNull(candidate.parent)', walker)
        limit = int(re.search(r'for \(depth in 0 until (\d+)\)', walker)[1])
        self.assertEqual(limit, 16)
        self.assertIn('if (candidate.className?.toString() == role)', walker)
        self.assertNotIn('candidate.isClickable ||', walker)
        self.assertNotIn('!candidate.isEnabled ||', walker)
        guard = walker.split('check(candidate.refresh() &&', 1)[1].split(')\n', 1)[0]
        guard = 'refreshed and ' + guard.replace('candidate.windowId', 'window').replace('candidate.packageName?.toString()', 'package').replace('targetContext.packageName', 'owned_package').replace('ownedWindow', 'owned_window').replace('&&', 'and')
        disabled = walker.split('check(!candidate.isEnabled &&', 1)[1].split(') {', 1)[0]
        disabled = 'not enabled and ' + disabled.replace('candidate.isVisibleToUser', 'visible')
        def resolve(nodes, role):
            for index, node in enumerate(nodes[:limit]):
                state = dict(owned_window=7, owned_package='app.zerus.mobile', parent=index + 1 if index + 1 < len(nodes) else None, **node)
                if not finite_source_predicate(guard, state): return None
                if node['role'] == role:
                    return index if finite_source_predicate(disabled, state) else None
                if state['parent'] is None: return None
            return None
        label = dict(refreshed=True, window=7, package='app.zerus.mobile', role='android.widget.TextView', enabled=True, visible=True)
        for action_name, role in roles.items():
            with self.subTest(action=action_name):
                action = {**label, 'role': role, 'enabled': False}
                self.assertEqual(resolve([label, action], role), 1)
                self.assertEqual(resolve([action], role), 0)
                self.assertIsNone(resolve([{**label, 'enabled': False}, {**action, 'enabled': True}], role))
                self.assertIsNone(resolve([{**label, 'enabled': False}], role))
                self.assertIsNone(resolve([{**action, 'role': 'android.view.View'}], role))
                self.assertIsNone(resolve([label] * limit + [action], role))
                for changed in ({'refreshed': False}, {'window': 8}, {'package': 'foreign'}, {'enabled': True}, {'visible': False},
                                {'role': next(value for value in roles.values() if value != role)}):
                    with self.subTest(changed=changed):
                        self.assertIsNone(resolve([label, {**action, **changed}], role))
                self.assertIsNone(resolve([label], role))
        self.assertIn('staleDisabledAction(choice, "Approve once") == choiceAction', source)
        self.assertIn('staleDisabledAction(label, "Send answer") == action', source)
        self.assertNotIn('injectInputEvent', walker)
        self.assertNotIn('performAction', walker)


class StaleComposeRoleSourceTests(unittest.TestCase):
    def source(self):
        return original_review_fixture()

    def test_option_only_seams_restore_every_byte_of_4233_and_the_send_walker(self):
        source = self.source()
        source = strip_stale_option_evidence(source)
        pattern = re.compile(r'^([ ]*)// BEGIN STALE_COMPOSE_ROLE ([a-z-]+)\n[\s\S]*?^\1// END STALE_COMPOSE_ROLE \2\n', re.M)
        self.assertEqual([m[2] for m in pattern.finditer(source)], ['definition', 'admission'])
        original = pattern.sub('', source)
        self.assertEqual(hashlib.sha256(original.encode()).hexdigest(),
                         'e61677c18f4ca75f8c78df130f0c6da8573af81524bc5770b273572465253f90')
        walker = source.split('            fun staleDisabledAction(', 1)[1].split('            val choice =', 1)[0]
        self.assertEqual(pattern.findall(walker)[0][1], 'admission')
        self.assertIn('if (depth == 0 && text == "Approve once") composeStaleOptionAction(label)?.let { return it }', walker)
        legacy = original.split('            fun staleDisabledAction(', 1)[1].split('            val choice =', 1)[0]
        self.assertEqual(pattern.sub('', walker), legacy)
        self.assertEqual(source.split('            val choice =', 1)[1], original.split('            val choice =', 1)[1])
        for forbidden in ('performAction', 'injectInputEvent', 'sleep(', 'await(', 'Thread(', 'sendStatus(', 'bindReview(', 'resumeEnd ='):
            self.assertNotIn(forbidden, pattern.search(source)[0])

    def test_pinned_synthetic_role_shape_has_a_source_bound_legacy_counterexample(self):
        source, f = self.source(), compose_option_nodes()
        walker = source.split('            fun staleDisabledAction(', 1)[1].split('            val choice =', 1)[0]
        comparison = re.search(r'if \((candidate.className\?\.toString\(\) == role)\)', walker)[1]
        comparison = comparison.replace('candidate.className?.toString()', 'actual')
        self.assertFalse(any(finite_source_predicate(comparison, dict(actual=n.role, role='android.widget.RadioButton'))
                             for n in (f.label, f.owner, f.root)))
        self.assertIs(compose_option_memory(source, f.label, {}), f.owner)
        # The synthetic source model succeeds with an enabled text label and enabled role-only witness.
        self.assertTrue(f.label.enabled and f.witness.enabled)
        self.assertFalse(f.owner.enabled)
        self.assertEqual(f.witness.actions, set())
        direct = compose_option_nodes(); direct.owner.role = 'android.widget.RadioButton'
        self.assertIsNone(compose_option_memory(source, direct.label, {}))  # Only the old walker may admit this shape.

    def test_source_bound_role_predicate_refuses_decorative_foreign_ambiguous_and_pseudo_roles(self):
        source = self.source()
        changes = [lambda f: setattr(f.owner, 'enabled', True),
            lambda f: setattr(f.owner, 'checkable', False),
            lambda f: setattr(f.owner, 'visible', False), lambda f: setattr(f.witness, 'bounds', (10, 100, 50, 150)),
            lambda f: setattr(f.witness, 'text', 'android.widget.RadioButton'),
            lambda f: setattr(f.witness, 'description', 'Approve once'),
            lambda f: setattr(f.witness, 'checkable', True), lambda f: setattr(f.witness, 'checked', True),
            lambda f: setattr(f.witness, 'selected', True),
            lambda f: setattr(f.witness, 'clickable', True), lambda f: setattr(f.witness, 'long_clickable', True),
            lambda f: setattr(f.witness, 'actions', {'click'}), lambda f: setattr(f.witness, 'actions', {'long-click'}),
            lambda f: setattr(f.witness, 'window', 10), lambda f: setattr(f.witness, 'package', 'foreign'),
            lambda f: setattr(f.witness, 'parent', f.root), lambda f: setattr(f.witness, 'refreshed', False),
            lambda f: setattr(f.label, 'text', 'Other choice'), lambda f: setattr(f.label, 'refreshed', False),
            lambda f: f.owner.children.append(f.node(role='android.widget.RadioButton', parent=f.owner, bounds=f.owner.bounds)),
            lambda f: setattr(f.owner, 'children', [f.label]),
            lambda f: setattr(f.owner, 'parent', f.owner)]
        for index, change in enumerate(changes):
            with self.subTest(refusal=index):
                f = compose_option_nodes(); change(f)
                try: result = compose_option_memory(source, f.label, {})
                except AssertionError: result = None
                self.assertIsNone(result)
        f = compose_option_nodes(); f.witness.text = 'pseudo role'
        self.assertIsNone(compose_option_memory(source, f.label, {}))
        # Mutating the actual source predicate changes this controlled result: it is not a hard-coded mirror.
        head, tail = source.split('fun roleOnly(child: AccessibilityNodeInfo): Boolean =', 1)
        changed = head + 'fun roleOnly(child: AccessibilityNodeInfo): Boolean =' + tail.replace('child.text.isNullOrEmpty() && ', '', 1)
        self.assertIs(compose_option_memory(changed, f.label, {}), f.owner)
        f.witness.actions = {'accessibility-focus'}; f.witness.text = ''
        self.assertIs(compose_option_memory(source, f.label, {}), f.owner)

    def test_two_parent_checked_removals_reverse_to_every_96fc_fixture_byte(self):
        source = self.source()
        original = restore_stale_owner_checked(restore_compose_role_children(source))
        self.assertEqual(hashlib.sha256(original.encode()).hexdigest(),
                         '9b35c3ca5a5a3e9839a34ba53410ab45957f83ada14f7bad30f8d81c7c6b5d53')
        self.assertEqual(restore_compose_role_children(source).count('!child.isCheckable && !child.isChecked && !child.isSelected'), 1)
        self.assertEqual(restore_compose_role_children(source).split('            fun staleDisabledAction(', 1)[1],
                         original.split('            fun staleDisabledAction(', 1)[1])

    def test_touch_target_reversal_restores_every_304b_byte(self):
        source = self.source()
        for exact in TOUCH_TARGET_SOURCE:
            self.assertEqual(source.count(exact), 1)
        self.assertEqual(hashlib.sha256(restore_compose_role_children(source).encode()).hexdigest(),
                         '217e641a736eeb45ff1faa41462c8ae4316b3231859bd59455841a4c0eba71da')

    def test_send_role_child_admission_is_pinned_bounded_and_refusal_logged(self):
        source = self.source()
        definition = source.split('            // BEGIN STALE_COMPOSE_ROLE definition\n', 1)[1].split(
            '            // END STALE_COMPOSE_ROLE definition', 1)[0]
        helper = definition.split('            fun composeStaleButtonAction(label: AccessibilityNodeInfo): AccessibilityNodeInfo? {\n', 1)[1].split(
            '            var composeOptionOwner:', 1)[0]
        for exact in SEND_ROLE_SOURCE:
            self.assertEqual(source.count(exact), 1, exact)
        self.assertIn('for (depth in 0 until 16)', helper)
        self.assertIn('check(children in 1..32)', helper)
        self.assertIn('if (accepted == null) try {', helper)
        self.assertIn('Log.i("ZerusApprovalUI", line.take(512))', helper)
        self.assertIn('if (facts.size >= 48) return', helper)
        for forbidden in ('performAction', 'injectInputEvent', 'sleep(', 'sendStatus(', 'staleOptionFailureEvidence'):
            self.assertNotIn(forbidden, helper)

    def observed_touch_target_nodes(self):
        # Hosted run 39638211299 retained these exact owner, role-child and label rectangles.
        f = compose_option_nodes()
        f.root.bounds = (0, 0, 1080, 2400)
        f.owner.checked = False
        f.owner.bounds = (74, 1192, 1006, 1318)
        f.witness.bounds = (74, 1197, 1006, 1312)
        f.label.bounds = (184, 1223, 455, 1286)
        return f

    def test_observed_compose_touch_target_is_admitted_and_was_refused_before(self):
        source = self.source()
        f = self.observed_touch_target_nodes()
        self.assertIs(compose_option_memory(source, f.label, {}), f.owner)
        f = self.observed_touch_target_nodes()
        try: result = compose_option_memory(restore_compose_role_children(source), f.label, {})
        except AssertionError: result = None
        self.assertIsNone(result)

    def test_touch_target_refuses_off_centre_unpadded_and_oversized_role_children(self):
        source = self.source()
        changes = {
            'off-centre': lambda f: setattr(f.witness, 'bounds', (74, 1192, 1006, 1307)),
            'owner-not-48dp': lambda f: (setattr(f.owner, 'bounds', (74, 1190, 1006, 1320)),
                                         setattr(f.witness, 'bounds', (74, 1197, 1006, 1312))),
            'narrower': lambda f: setattr(f.witness, 'bounds', (80, 1197, 1000, 1312)),
            'oversized': lambda f: setattr(f.witness, 'bounds', (74, 1190, 1006, 1320)),
            'empty': lambda f: setattr(f.witness, 'bounds', (74, 1255, 1006, 1255)),
        }
        for name, change in changes.items():
            with self.subTest(case=name):
                f = self.observed_touch_target_nodes(); change(f)
                try: result = compose_option_memory(source, f.label, {})
                except AssertionError: result = None
                self.assertIsNone(result)

    def test_both_parent_selection_states_need_the_same_role_and_actual_fresh_predicate(self):
        # Source-bound finite stand-in, not compiled Kotlin or an observed child hierarchy.
        source = self.source()
        for checked in (False, True):
            f, binding = compose_option_nodes(), {}
            f.owner.checked = checked
            with self.subTest(parent_checked=checked):
                self.assertIs(compose_option_memory(source, f.label, binding), f.owner)
                self.assertIs(compose_option_memory(source, f.label, binding), f.owner)
                f.witness.checked = True
                self.assertIsNone(compose_option_memory(source, f.label, {}))
        old = restore_stale_owner_checked(source)
        only_admission_fixed = source.replace('found.first.isCheckable &&\n',
            'found.first.isCheckable && found.first.isChecked &&\n', 1)
        only_fresh_fixed = source.replace('!owner.isEnabled && owner.isCheckable) {',
            '!owner.isEnabled && owner.isCheckable && owner.isChecked) {', 1)
        for name, candidate in (('old', old), ('only-admission', only_admission_fixed), ('only-fresh', only_fresh_fixed)):
            f = compose_option_nodes(); f.owner.checked = False
            with self.subTest(correction=name):
                try: result = compose_option_memory(candidate, f.label, {})
                except AssertionError: result = None
                self.assertIsNone(result)

    def test_complete_child_and_ancestor_bounds_and_unknown_current_cannot_admit(self):
        source = self.source()
        for kind in ('children', 'unknown-child', 'ancestors', 'ambiguous-owner', 'outside', 'foreign-sibling'):
            f = compose_option_nodes()
            if kind == 'children':
                f.owner.children.extend(f.node(parent=f.owner) for _ in range(31))
            elif kind == 'unknown-child': f.owner.children.append(None)
            elif kind == 'ancestors':
                tail = f.root
                for _ in range(16):
                    tail.parent = f.node(); tail = tail.parent
            elif kind == 'ambiguous-owner':
                f.root.enabled = False; f.root.checkable = f.root.checked = True
                f.root.children.append(f.node(role='android.widget.RadioButton', parent=f.root, bounds=f.root.bounds))
            elif kind == 'outside': f.label.bounds = (0, 0, 1000, 500)
            else:
                f.owner.children = [f.label]; f.witness.parent = f.root; f.root.children.append(f.witness)
            with self.subTest(kind=kind):
                try: result = compose_option_memory(source, f.label, {})
                except AssertionError: result = None
                self.assertIsNone(result)
        for values in ((False,), (True, False)):
            f = compose_option_nodes(); states = iter(values)
            with self.assertRaises(AssertionError): compose_option_memory(source, f.label, {}, lambda: next(states))

    def test_final_proof_rejects_replaced_witness_owner_or_label_geometry(self):
        source = self.source()
        for changed in ('witness', 'owner', 'geometry', 'role-shape'):
            f, binding = compose_option_nodes(), {}
            self.assertIs(compose_option_memory(source, f.label, binding), f.owner)
            if changed == 'witness':
                f.owner.children[1] = f.node(role='android.widget.RadioButton', parent=f.owner, bounds=f.owner.bounds)
            elif changed == 'owner':
                replacement = f.node(enabled=False, checkable=True, checked=True, parent=f.root, bounds=f.owner.bounds)
                replacement.children = [f.label, f.witness]; f.label.parent = f.witness.parent = replacement
            elif changed == 'geometry': f.label.bounds = (90, 145, 390, 195)
            else: f.owner.role = 'android.widget.RadioButton'
            with self.subTest(changed=changed), self.assertRaises(AssertionError):
                compose_option_memory(source, f.label, binding)

    def test_failure_descriptors_are_bounded_observations_with_no_admission_effect(self):
        source = self.source()
        helper = source.split('            fun composeStaleOptionAction(', 1)[1].split('            // END STALE_COMPOSE_ROLE definition', 1)[0]
        self.assertIn('if (facts.size >= 48) { factsOverflow = true; return }', helper)
        self.assertIn('node.className?.toString()?.take(120)', helper)
        self.assertIn('if (accepted == null && !directRole) try {', helper)
        self.assertIn('stale-option-node UNKNOWN descriptor-cap-or-fault', helper)
        self.assertEqual(helper.count('Log.i('), 2)
        fact = helper.split('fun fact(', 1)[1].split('                fun roleOnly(', 1)[0]
        for forbidden in ('node.text', 'node.contentDescription', 'main {', 'refresh()', 'performAction', 'injectInputEvent'):
            self.assertNotIn(forbidden, fact)
        self.assertIn('finally {', helper)
        self.assertNotIn('catch (error:', helper)

    def test_retained_diagnostic_seams_restore_all_9a4_bytes_and_original_throwable_path(self):
        source = self.source()
        original = restore_compose_role_children(strip_stale_option_evidence(source))
        # This diagnostic projection includes only the two corrected parent predicates; the full 96fc reverse is separate.
        self.assertEqual(hashlib.sha256(original.encode()).hexdigest(),
                         'ec824f50ae31c873d76d9c60b9f946366ceab85ccaa5cd040f67f0c0111a732d')
        walker = lambda value: value.split('            fun staleDisabledAction(', 1)[1].split('            val choice =', 1)[0]
        self.assertEqual(walker(restore_compose_role_children(source)), walker(original))
        passed = next(line for line in source.splitlines() if 'Approval choices smoke passed:' in line)
        self.assertNotIn('staleOptionFailureEvidence', passed)
        failure_line = lambda value: next(line for line in value.splitlines() if 'result.putString("stream", "Approval choices smoke failed:' in line)
        self.assertEqual(failure_line(source), failure_line(original))
        publication = source.split('// BEGIN STALE_OPTION_EVIDENCE failure-stream\n', 1)[1].split('// END STALE_OPTION_EVIDENCE failure-stream', 1)[0]
        self.assertIn('val addition = "\\n" + staleOptionFailureEvidence', publication)
        self.assertIn('check(addition.length <= 25_600)', publication)
        self.assertLess(publication.index('check(addition.length'), publication.index('val retained = failureEvidence + addition'))
        self.assertLess(publication.index('val retained = failureEvidence + addition'), publication.index('failureEvidence = retained'))
        self.assertEqual(publication.count('failureEvidence ='), 1)
        for forbidden in ('refresh(', 'getChild(', 'SystemClock', 'main {', 'Log.', 'throw ', 'error.', 'sendStatus'):
            self.assertNotIn(forbidden, publication)
        helper = source.split('            fun composeStaleOptionAction(', 1)[1].split('// END STALE_COMPOSE_ROLE definition', 1)[0]
        self.assertEqual(helper.count('diagnosticCompleted = true'), 2)
        self.assertIn('diagnosticCompleted = true\n                            // END STALE_OPTION_EVIDENCE delegate-complete\n                            return null', helper)
        self.assertIn('diagnosticCompleted = true\n                    // END STALE_OPTION_EVIDENCE return-complete\n                    return accepted', helper)
        publish = helper.split('// BEGIN STALE_OPTION_EVIDENCE publish\n', 1)[1].split('// END STALE_OPTION_EVIDENCE publish', 1)[0]
        for forbidden in ('refresh(', 'getChild(', 'getBounds', 'className', 'SystemClock', 'main {', 'Log.',
                          'roleOnly(', 'staleInspectionCurrent(', 'performAction', 'injectInputEvent', 'sendStatus'):
            self.assertNotIn(forbidden, publish)
        self.assertLess(helper.index('staleOptionFailureEvidence = captured'), helper.index('for (line in facts) Log.i('))

    def test_source_bound_memory_retains_null_direct_and_accepted_paths_without_logcat(self):
        source = self.source()
        facts = ['stale-option-node kind=ancestor index=0 class=android.widget.TextView',
                 'stale-option-node kind=ancestor index=1 class=android.view.View']
        for direct, accepted, expected in ((False, None, 'NO_OWNER'), (True, None, 'DIRECT_ROLE_DELEGATED'),
                                           (False, object(), 'OWNER_ACCEPTED')):
            with self.subTest(outcome=expected):
                captured = stale_option_evidence_memory(source, facts, completed=True, direct=direct, accepted=accepted)
                self.assertIn('status=observed outcome=' + expected, captured)
                self.assertTrue(all(line in captured for line in facts))
                # A controlled dropped/throwing logger has no route back into this already copied memory value.
                try: raise OSError('controlled log sink failure')
                except OSError: pass
                self.assertTrue(all(line in captured for line in facts))
        interrupted = stale_option_evidence_memory(source, facts, completed=False)
        self.assertIn('status=UNKNOWN outcome=UNKNOWN_INTERRUPTED', interrupted)

    def test_source_bound_diagnostic_caps_and_copy_fault_preserve_failure_and_mark_unknown(self):
        source = self.source()
        captured = stale_option_evidence_memory(source, ['x' * 513] * 49, completed=True)
        self.assertLessEqual(len(captured), 25_600)
        self.assertIn('status=UNKNOWN', captured)
        self.assertIn('rows=48', captured)
        self.assertEqual(captured.count('UNKNOWN cap-or-fault'), 1)
        self.assertFalse(any(len(line) > 512 for line in captured.splitlines()))
        self.assertIn('status=UNKNOWN', stale_option_evidence_memory(source, ['partial'], completed=True, overflow=True))
        original = RuntimeError('controlled original refusal')
        try:
            try: raise original
            finally: captured = stale_option_evidence_memory(source, [], completed=False, capture_fault=True)
        except RuntimeError as observed:
            self.assertIs(observed, original)
        self.assertEqual(captured, 'stale-option-evidence status=UNKNOWN outcome=CAPTURE_FAULT')

    def test_stale_open_phase_seams_restore_every_f556_byte_and_all_earlier_observers(self):
        source = self.source()
        original = restore_compose_role_children(strip_stale_open_phases(source))
        # This phase projection includes only the two corrected parent predicates; the full 96fc reverse is separate.
        self.assertEqual(hashlib.sha256(original.encode()).hexdigest(),
                         '71a90f9f9776fd2b485498f0d2e40ad616efcd749d9075e91775e0c74d4bd135')
        self.assertEqual(restore_compose_role_children(source).split('            // BEGIN STALE_COMPOSE_ROLE definition', 1)[1],
                         original.split('            // BEGIN STALE_COMPOSE_ROLE definition', 1)[1])
        self.assertEqual(source.split('        fun open(', 1)[0], original.split('        fun open(', 1)[0])

    def test_stale_open_six_fixed_markers_bracket_original_calls_only_for_one_name(self):
        source = self.source()
        opening = source.split('        fun open(', 1)[1].split('        fun reviewed()', 1)[0]
        pattern = re.compile(r'^([ ]*)// BEGIN STALE_OPEN_PHASE ([a-z-]+)\n([\s\S]*?)^\1// END STALE_OPEN_PHASE \2\n', re.M)
        seams = list(pattern.finditer(opening))
        selector = re.search(r'val traceStaleOpen = ([^\n]+)', opening)[1]
        self.assertEqual(selector, 'name == "stale-identity"')
        names = re.findall(r'open\("([^"\n]+)"', source)
        self.assertEqual(names.count('stale-identity'), 1)
        for name in names:
            self.assertEqual(finite_source_predicate(selector, dict(name=name)), name == 'stale-identity')
        markers = []
        for seam in seams:
            body = seam[3]
            match = re.search(r'if \(traceStaleOpen\) try \{ phase\("(stale-open:[A-Z_]+)"\) } catch \(_: Throwable\) \{ }', body)
            self.assertIsNotNone(match)
            markers.append(match[1])
            for forbidden in ('main {', 'node(', 'await(', 'refresh(', 'getChild(', 'performAction', 'injectInputEvent',
                              'sendStatus(', 'sleep(', 'Thread(', 'model.'):
                self.assertNotIn(forbidden, body)
        self.assertEqual(markers, ['stale-open:REFRESH_MAIN_BEFORE', 'stale-open:REFRESH_MAIN_RETURNED',
            'stale-open:TITLE_BEFORE', 'stale-open:TITLE_RETURNED', 'stale-open:VERIFIED_BEFORE', 'stale-open:VERIFIED_RETURNED'])
        calls = ['main { model.refreshActivity() }', 'node("Review synthetic $name")',
            'await("fresh approval $name") { main { model.activityVerified && model.conversationQuestions.singleOrNull()?.id == name } }']
        for index, call in enumerate(calls):
            self.assertEqual(opening.count(call), 1)
            self.assertLess(seams[index * 2].end(), opening.index(call))
            self.assertLess(opening.index(call) + len(call), seams[index * 2 + 1].start())
        self.assertEqual(opening.count('phase("stale-open:'), 6)


class QuestionLayoutSourceTests(unittest.TestCase):
    def test_question_scroll_hint_reserves_space_outside_weighted_disclosure(self):
        source = (ROOT / 'mobile/android/app/src/main/java/app/zerus/mobile/QuestionUi.kt').read_text()
        self.assertEqual(question_scroll_hint_findings(source), [])

    def test_layout_scope_guard_ignores_comments_and_quoted_braces(self):
        source = 'Column { // }\n Text("}{"); val character = \'}\'; /* } { */ val raw = """ } { """\n Box { Text("inside") }\n}'
        outer = source.index('{')
        self.assertEqual(kotlin_brace_scopes(source, source.index('Box')), (outer,))
        self.assertEqual(kotlin_brace_scopes(source, source.index('Text("inside")')), (outer, source.index('{', source.index('Box'))))

    def test_layout_scope_guard_refuses_overlay_nested_and_zero_height_hints(self):
        column = 'Column(Modifier.then(if(inDialog) Modifier.fillMaxSize() else Modifier.heightIn(max = contentLimit))) {'
        box = 'Box(Modifier.fillMaxWidth().weight(1f, fill = inDialog).then(disclosureVisibility)) {'
        hint = 'if (bodyScroll.canScrollForward) Surface(modifier = Modifier.align(Alignment.CenterHorizontally)) { Text("Scroll for options ↓") }'
        footer = 'otherPrompts.forEachIndexed { index,(prompt,field) -> Text("footer") }'
        reserved = column + box + 'Text("options") }' + hint + footer + '}'
        self.assertEqual(question_scroll_hint_findings(reserved), [])
        overlay = column + box + 'Text("options") ' + hint + '}' + footer + '}'
        self.assertIn('hint-overlaps-weighted-disclosure', question_scroll_hint_findings(overlay))
        nested = column + box + 'Column { Text("options") ' + hint + '}}' + footer + '}'
        self.assertIn('hint-must-reserve-direct-column-space', question_scroll_hint_findings(nested))
        zero = reserved.replace('Alignment.CenterHorizontally)', 'Alignment.CenterHorizontally).height(0.dp)')
        self.assertIn('hint-must-retain-measured-sibling-size', question_scroll_hint_findings(zero))


if __name__ == "__main__":
    unittest.main()
