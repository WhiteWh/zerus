#!/usr/bin/env python3
"""Authored SDK-free source contracts; not Kotlin or runtime qualification."""
import hashlib
import importlib.util
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]
_focus_spec = importlib.util.spec_from_file_location("review_focus_projection", ROOT / "tests/review_focus_projection.py")
_focus_projection = importlib.util.module_from_spec(_focus_spec)
_focus_spec.loader.exec_module(_focus_projection)
original_review_fixture = _focus_projection.original_review_fixture
# Fixture pin includes one-time post-resume live-label preparation; the approval harness retains exact 95ef by reverse projection.
MAIN = ROOT / "mobile/android/app/src/main/java/app/zerus/mobile"
# Keep the reviewed 9b217ddd/b9c1e29c reconstruction pins unchanged. The exact
# fixtures first reverse the outbox-retention and send-receipt additions, then
# the send-entry and transport-evidence additions, then
# presentation, delivery recovery, recent-Activity, child-session and
# visible-refresh additions, then the reviewed task-tracker additions to 86aeab6d, then
# the composed workspace and push-registration changes to ac601e36,
# then the 962699e6 read-catalog/badge update and c1eebe96 mobile checkout
# additions. None changes question eligibility or the ten verification writes.
# No Git history or Android SDK is needed at run time.
MODEL_SHA = "a94c04695da3626c49f1cc5a875c82f8b3893688d256c7cfa3ebf66cad9e619a"
MAIN_SHA = "7c676d33a2218da83511697f5a597c4876b946e1c130d601c265e855b7259f4b"
UPDATES = ROOT / "tests/fixtures/question-availability-reviewed-updates.patch"
UPDATES_SHA = "5088def68877668c9a7e261851d552cd30b0fee1806aa072a911172f6ff4f60a"
WORKSPACE_UPDATES = ROOT / "tests/fixtures/question-availability-workspace-updates.patch"
WORKSPACE_UPDATES_SHA = "ff2e0ae7ccf5f14a49be075a38978757f453e59f2461fda329d3d8b51a9d0443"
# Exact 86aeab6d -> 53170eee feature delta; policy and baseline pins stay fixed.
TASK_TRACKER_UPDATES = ROOT / "tests/fixtures/question-availability-task-tracker-updates.patch"
TASK_TRACKER_UPDATES_SHA = "5cb5f076e3f176069d8e57d945f1286cea7763fcf37c3dbfb6f61383cf5c1e34"
# Exact visible-refresh, foreground priority and frame-observer delta, then the
# fork inherited-history rows (zerus-b4qk.1, first in the MainActivity section);
# keep historical policy pins.
VISIBLE_REFRESH_UPDATES = ROOT / "tests/fixtures/question-availability-visible-refresh-updates.patch"
VISIBLE_REFRESH_UPDATES_SHA = "5c6042fa033ffb1f75bcac2136cc94eff5b895fa144be4e2f325661edb04e797"
# Exact child-session delta to these two files: #1722 (d7c70e41e, merge
# 88b9b6060) and its follow-up #1723 (bd7ded090, merge 96d8df5dc). Nested
# child cards, quiet supervised children, account marks and the collapse
# store; none changes question eligibility or the ten verification writes
# (zerus-e3buj).
CHILD_SESSION_UPDATES = ROOT / "tests/fixtures/question-availability-child-session-updates.patch"
CHILD_SESSION_UPDATES_SHA = "c97d445dacc9121bad6f7618c2938a699d5a6cd1364c9f9d05ac959ad5937fc7"
# Exact two-file delta in 16a21484f, after reviewing its fresh-inspection identity,
# presentation ownership and unchanged question/send gates (zerus-l8ldt). Reverse
# it after newer reviewed updates; keep all original policy, verification-writer and workspace pins.
RECENT_ACTIVITY_UPDATES = ROOT / "tests/fixtures/question-availability-recent-activity-updates.patch"
RECENT_ACTIVITY_UPDATES_SHA = "05f9cb60d46c095e91561d5bdfe6c7d76c850218fd6ca9b44958ffebdb7e730c"
# Exact local delivery-dismissal UI, durable recovery and bounded send/read delta
# from 859cb4f (zerus-a4mub.2, .7). Question eligibility, native availability,
# every verification assignment and all historical reconstruction pins stay exact.
DELIVERY_RECOVERY_UPDATES = ROOT / "tests/fixtures/question-availability-delivery-recovery-updates.patch"
DELIVERY_RECOVERY_UPDATES_SHA = "9606f2de9579fb7100e4ca0878a4574cb919a651bf37743cee23177674b8a25e"
# Exact two-file presentation delta 1e41c497d..e14859381: 288a92bf5 (quiet sync
# status, stable Activity row keys and loaded-unread counts) and e14859381 (saved
# positions through presentation aliases). It replaces the refresh status banner
# with read timing only. Question eligibility, native availability, every
# verification assignment and all historical reconstruction pins stay exact
# (zerus-p7i3o). Reverse it after newer reviewed updates.
PRESENTATION_UPDATES = ROOT / "tests/fixtures/question-availability-presentation-updates.patch"
PRESENTATION_UPDATES_SHA = "dde1e6eb48faf7d29a8407726be4cf74b176e95802fd1249bb95ee49dc9f8003"
# Exact outbox transport-evidence delta of #2041 (zerus-a4mub.6): the send
# entry records the stage of its mutation attempt and settles by it instead of
# guessing from an exception type. One reviewed file; question eligibility,
# native availability, every verification assignment and all historical
# reconstruction pins stay exact. Reverse it first on the #2041 head; the later
# #2044 send-entry delta reverses before it when both are present.
# Exact outbox send-entry wiring delta of #2044 (zerus-a4mub.21): the send
# evidence is required by type through RelayApi and OutgoingSend owns the send
# entry. One reviewed file; question eligibility, native availability, every
# verification assignment and all historical pins stay exact. Reverse it first,
# before the #2041 evidence delta it builds on.
# Exact question-projection completeness delta of zerus-a4mub.27: a verified
# projection that is still indexing (pending_questions_complete=false) can never
# close or replace an expansion; an older CLI omits the field and stays
# complete. One reviewed file; all historical reconstruction pins stay exact.
# Reverse it first, before the presence delta it extends.
QUESTION_PROJECTION_COMPLETE_UPDATES = ROOT / "tests/fixtures/question-availability-question-projection-complete-updates.patch"
QUESTION_PROJECTION_COMPLETE_UPDATES_SHA = "1209c0704fc6ae050db05e274127450bae3480c8bb42efa79ec9ff888cfdbcfe"
QUESTION_PROJECTION_COMPLETE_FILES = ("ZerusViewModel.kt",)
# Exact expanded-question presence delta of zerus-a4mub.12: the verified
# question projection records when it was last checked, so an obsolete
# expansion can show its check age instead of a refresh demand. One reviewed
# file; question eligibility, native availability, every verification writer
# and all historical reconstruction pins stay exact. Reverse it after newer reviewed updates.
QUESTION_PRESENCE_UPDATES = ROOT / "tests/fixtures/question-availability-question-presence-updates.patch"
QUESTION_PRESENCE_UPDATES_SHA = "d9dab9d7203e2142c97440db106f8ade2b0b7dc9ffdcfd1db20db254cf38d008"
QUESTION_PRESENCE_FILES = ("ZerusViewModel.kt",)
# Exact Android outbox follow-up delta of zerus-a4mub.22/.23: a send proven
# Not sent before the transport marker releases its sealed reservation and the
# outbox shows its allowlisted local reason. Two reviewed files; question
# eligibility, native availability, every verification assignment and all
# historical reconstruction pins stay exact. Reverse it after newer reviewed updates.
OUTBOX_RELEASE_REASON_UPDATES = ROOT / "tests/fixtures/question-availability-outbox-release-reason-updates.patch"
OUTBOX_RELEASE_REASON_UPDATES_SHA = "fd8c60cc4c63947e781283532f12e0153a9bb5e95373e5313d3f4cb987834505"
OUTGOING_SEND_UPDATES = ROOT / "tests/fixtures/question-availability-outgoing-send-updates.patch"
OUTGOING_SEND_UPDATES_SHA = "a65e1fc9cdb0862b597c3c732c48d3c1e7f01d957bf06569c02e5b3042205334"
OUTGOING_SEND_FILES = ("ZerusViewModel.kt",)
SEND_ATTEMPT_UPDATES = ROOT / "tests/fixtures/question-availability-send-attempt-updates.patch"
SEND_ATTEMPT_UPDATES_SHA = "001897f22af91535bc1f88cec7588eff75336a07081f1cb35fe95a5f0078730b"
SEND_ATTEMPT_FILES = ("ZerusViewModel.kt",)
# Exact two-file conversation-status delta cea24c0f4..8e344939a (zerus-a4mub.10):
# one time-bound status strip replaces the Connected/Last known tag, the
# bounded-window list item and the conversation's own global read-failure
# banner; the model publishes the shown frame's observation time, the read
# failure's kind and observed shared-read outcomes. Presentation only: question
# eligibility, native availability, every verification writer and all historical
# reconstruction pins stay exact. Reverse it after newer reviewed updates.
SYNC_STATUS_UPDATES = ROOT / "tests/fixtures/question-availability-sync-status-updates.patch"
SYNC_STATUS_UPDATES_SHA = "fc55b33f20190f4ac88fa7012c03a304d855caeb2f238ce9f39f9a4b6a70bf77"
# Exact two-file current-read delta babdce3c1..3220a3158 (zerus-a4mub.9, #2060):
# the open conversation acknowledges its verified inspection's current reply and
# attention after the settled visibility dwell (tail, newest row and questions
# flags, the current values' dwell key), explicit Mark read shares them, and every
# shared mark goes through one launchSharedRead, which now holds #2053's Pending
# note. Shared reads only: question eligibility, native availability, every
# verification writer and all historical reconstruction pins stay exact. Reverse
# it after newer reviewed updates.
CURRENT_READ_UPDATES = ROOT / "tests/fixtures/question-availability-current-read-updates.patch"
CURRENT_READ_UPDATES_SHA = "1f1e6f291f1568fdf2d6fecb7847c3bff9826d7925e8e8422e2bbb5ef93d5ea5"
# Exact two-file current-read flags delta d2fa7b274..417ed2404 (zerus-a4mub.30, #2066):
# the settled layout reaches one CurrentFlags value through
# ConversationViewport.currentFlags/visibleEventIds, and the view model passes it
# whole to SharedReadPolicies.currentShown. Shared reads only: question eligibility,
# native availability, every verification writer and all historical reconstruction
# pins stay exact. Reverse it first, as the newest reviewed update.
CURRENT_READ_FLAGS_UPDATES = ROOT / "tests/fixtures/question-availability-current-read-flags-updates.patch"
CURRENT_READ_FLAGS_UPDATES_SHA = "773de1b95877011a63741d2c8ce795cb7a1c1a062b041cf10988f2059c141289"
# Exact one-file readable-visibility delta 10f3a8455..6b147ec95 (zerus-a4mub.31, #2069): the
# settled layout passes the shared readable height in pixels to
# ConversationViewport.visibleEventIds and currentFlags. Visibility for reading only:
# question eligibility, native availability, every verification writer and all
# historical reconstruction pins stay exact. Reverse it first, as the newest reviewed update.
READABLE_VISIBILITY_UPDATES = ROOT / "tests/fixtures/question-availability-readable-visibility-updates.patch"
READABLE_VISIBILITY_UPDATES_SHA = "37238564a4fd15832d0dcd543462939d2708d1d497a23ed202b9477040c15dc4"
READABLE_VISIBILITY_FILES = ("MainActivity.kt",)
# Exact send-receipt delta of #2087 (zerus-a4mub.4.3) as merged in batch 7
# (e2e3526e9): a text-only send freezes its exact original send intent, and a
# receipt settles it only from that intent's acknowledgement, under a deadline.
# One reviewed file; it changes settleOutgoing's signature, a context line of
# the older outbox-release delta, so reverse it before that delta. Question
# eligibility, native availability, every verification writer and all
# historical reconstruction pins stay exact.
SEND_RECEIPT_UPDATES = ROOT / "tests/fixtures/question-availability-send-receipt-updates.patch"
SEND_RECEIPT_UPDATES_SHA = "d2266323766b80c1425252796d6f06735d6f38aa80fa7023918c1b4fbfc0a648"
SEND_RECEIPT_FILES = ("ZerusViewModel.kt",)
# Exact outbox-retention delta of #2118 (zerus-a4mub.47) as merged in batch 7
# (a4f0e6aa9): the outbox retains settled send identities compactly and bounds
# redundant rows on every outbox write, and a draft review records its
# coverage. One reviewed file, built on the #2087 send-receipt delta; reverse it
# first, as the newest reviewed update. Question eligibility, native
# availability, every verification writer and all historical pins stay exact.
OUTBOX_RETENTION_UPDATES = ROOT / "tests/fixtures/question-availability-outbox-retention-updates.patch"
OUTBOX_RETENTION_UPDATES_SHA = "fcf4a926f61bfb059fd4bf1efbcd934dd76b4d6a85ef11ccaed0f0bc3618bebc"
OUTBOX_RETENTION_FILES = ("ZerusViewModel.kt",)
WORKSPACE_BASE_SHA = {
    "ZerusViewModel.kt": "5932d92ca74ccf37da9595a580bcb1457c93c83951976606c8defa8d1ee83326",
    "MainActivity.kt": "f68902349d1e0ac939582e9e139548663f5763c430a3c566f8823d9f045645f0",
}
BOTH_FILES = ("ZerusViewModel.kt", "MainActivity.kt")
# Every reviewed update, newest first: reverse_reviewed_updates reverses them in
# this order, and each fixture's duplicated-hunk check reverses the newer ones.
REVIEWED_CHAIN = (
    (OUTBOX_RETENTION_UPDATES, OUTBOX_RETENTION_UPDATES_SHA, OUTBOX_RETENTION_FILES),
    (SEND_RECEIPT_UPDATES, SEND_RECEIPT_UPDATES_SHA, SEND_RECEIPT_FILES),
    (READABLE_VISIBILITY_UPDATES, READABLE_VISIBILITY_UPDATES_SHA, READABLE_VISIBILITY_FILES),
    (CURRENT_READ_FLAGS_UPDATES, CURRENT_READ_FLAGS_UPDATES_SHA, BOTH_FILES),
    (CURRENT_READ_UPDATES, CURRENT_READ_UPDATES_SHA, BOTH_FILES),
    (QUESTION_PROJECTION_COMPLETE_UPDATES, QUESTION_PROJECTION_COMPLETE_UPDATES_SHA, QUESTION_PROJECTION_COMPLETE_FILES),
    (QUESTION_PRESENCE_UPDATES, QUESTION_PRESENCE_UPDATES_SHA, QUESTION_PRESENCE_FILES),
    (SYNC_STATUS_UPDATES, SYNC_STATUS_UPDATES_SHA, BOTH_FILES),
    (OUTBOX_RELEASE_REASON_UPDATES, OUTBOX_RELEASE_REASON_UPDATES_SHA, BOTH_FILES),
    (OUTGOING_SEND_UPDATES, OUTGOING_SEND_UPDATES_SHA, OUTGOING_SEND_FILES),
    (SEND_ATTEMPT_UPDATES, SEND_ATTEMPT_UPDATES_SHA, SEND_ATTEMPT_FILES),
    (PRESENTATION_UPDATES, PRESENTATION_UPDATES_SHA, BOTH_FILES),
    (DELIVERY_RECOVERY_UPDATES, DELIVERY_RECOVERY_UPDATES_SHA, BOTH_FILES),
    (RECENT_ACTIVITY_UPDATES, RECENT_ACTIVITY_UPDATES_SHA, BOTH_FILES),
    (CHILD_SESSION_UPDATES, CHILD_SESSION_UPDATES_SHA, BOTH_FILES),
    (VISIBLE_REFRESH_UPDATES, VISIBLE_REFRESH_UPDATES_SHA, BOTH_FILES),
    (TASK_TRACKER_UPDATES, TASK_TRACKER_UPDATES_SHA, BOTH_FILES),
    (WORKSPACE_UPDATES, WORKSPACE_UPDATES_SHA, BOTH_FILES),
    (UPDATES, UPDATES_SHA, BOTH_FILES),
)
WRITERS = ["INSTALL_WAIT_PRESENTATION", "INSTALL_PRESENTATION", "PREPARE_PRESENTATION", "TERMINAL_CLOSED",
           "ACTION_RESULT", "CONNECTION_FORGOTTEN", "OPEN_SESSION", "BACK", "CLEAR_UNCONFIRMED", "INSPECT_FAILED"]


def strip_availability(source):
    source = re.sub(r"^    // BEGIN QUESTION_AVAILABILITY_PROBE ([a-z-]+)\n[\s\S]*?^    // END QUESTION_AVAILABILITY_PROBE \1\n", "", source, flags=re.M)
    return re.sub(r" /\* BEGIN QUESTION_AVAILABILITY_PROBE (writer-[a-z-]+) \*/[\s\S]*?/\* END QUESTION_AVAILABILITY_PROBE \1 \*/", "", source)


def reverse_reviewed_patch(name, source, fixture, fixture_sha,
                           reviewed_files=("ZerusViewModel.kt", "MainActivity.kt")):
    """Reverse exact reviewed hunks, never an arbitrary function/line range.

    A reviewed change declares the exact source files it touched; asking for a
    file it did not touch leaves the source unchanged (a one-file reviewed
    change is not an incomplete fixture)."""
    raw = fixture.read_bytes()
    if hashlib.sha256(raw).hexdigest() != fixture_sha:
        raise AssertionError("reviewed source-update fixture changed")
    prefix = "mobile/android/app/src/main/java/app/zerus/mobile/"
    files = {}
    for section in raw.decode().split("diff --git ")[1:]:
        header, body = section.split("\n", 1)
        before, after = header.split()
        filename = before.removeprefix("a/" + prefix)
        if (filename not in ("ZerusViewModel.kt", "MainActivity.kt")
                or before != "a/" + prefix + filename or after != "b/" + prefix + filename
                or filename in files):
            raise AssertionError("unexpected source-update fixture path")
        hunks = []
        for hunk in re.split(r"(?m)^@@[^\n]*\n", body)[1:]:
            lines = hunk.splitlines(keepends=True)
            if not lines or any(line[:1] not in (" ", "+", "-") for line in lines):
                raise AssertionError("unsupported source-update fixture hunk")
            old = "".join(line[1:] for line in lines if line[0] in " -")
            new = "".join(line[1:] for line in lines if line[0] in " +")
            if not old or not new or old == new:
                raise AssertionError("empty source-update fixture hunk")
            hunks.append((old, new))
        if not hunks:
            raise AssertionError("missing source-update fixture hunks")
        files[filename] = hunks
    if set(files) != set(reviewed_files):
        raise AssertionError("incomplete source-update fixture")
    if name not in reviewed_files:
        return source
    for old, new in files[name]:
        if source.count(new) != 1:
            raise AssertionError("reviewed source-update hunk is missing, changed or duplicated")
        source = source.replace(new, old, 1)
    return source


def reviewed_hunk(name, fixture, fixture_sha, addition):
    """The new text of the one reviewed hunk of `fixture` in `name` whose added
    lines contain `addition`, read as reverse_reviewed_patch reads hunks; a
    refusal test places it twice."""
    raw = fixture.read_bytes()
    if hashlib.sha256(raw).hexdigest() != fixture_sha:
        raise AssertionError("reviewed source-update fixture changed")
    found = []
    for section in raw.decode().split("diff --git ")[1:]:
        header, body = section.split("\n", 1)
        if header.split()[0].rsplit("/", 1)[-1] != name:
            continue
        for hunk in re.split(r"(?m)^@@[^\n]*\n", body)[1:]:
            lines = hunk.splitlines(keepends=True)
            if addition in "".join(line[1:] for line in lines if line[0] == "+"):
                found.append("".join(line[1:] for line in lines if line[0] in " +"))
    if len(found) != 1:
        raise AssertionError("no single reviewed hunk adds this text")
    return found[0]


def reverse_reviewed_updates(name, source):
    for fixture, fixture_sha, files in REVIEWED_CHAIN:
        source = reverse_reviewed_patch(name, source, fixture, fixture_sha, files)
    return source


def reconstructed(name, source):
    return strip_availability(reverse_reviewed_updates(name, source))


def original_source(name, source):
    try:
        digest = hashlib.sha256(reconstructed(name, source).encode()).hexdigest()
    except AssertionError:
        return False
    return digest == {"ZerusViewModel.kt": MODEL_SHA, "MainActivity.kt": MAIN_SHA}[name]


class QuestionAvailabilityProbeSourceTests(unittest.TestCase):
    def assert_refuses_duplicated_hunk(self, name, fixture, addition):
        """A whole reviewed hunk of `fixture` present twice, at the source state
        just before that fixture is reversed, is refused by its own reversal,
        never reversed once. (Duplicating a line inside a hunk only changes it.)"""
        at = [entry[0] for entry in REVIEWED_CHAIN].index(fixture)
        source = (MAIN / name).read_text()
        for newer, newer_sha, newer_files in REVIEWED_CHAIN[:at]:
            source = reverse_reviewed_patch(name, source, newer, newer_sha, newer_files)
        _, fixture_sha, files = REVIEWED_CHAIN[at]
        hunk = reviewed_hunk(name, fixture, fixture_sha, addition)
        self.assertEqual(source.count(hunk), 1)
        with self.subTest(source=name, kind="duplicated hunk", fixture=fixture.name):
            with self.assertRaises(AssertionError):
                reverse_reviewed_patch(name, source.replace(hunk, hunk + hunk, 1), fixture, fixture_sha, files)

    def test_workspace_projection_matches_reviewed_current_main_exactly(self):
        for name, digest in WORKSPACE_BASE_SHA.items():
            with self.subTest(source=name):
                current = reverse_reviewed_patch(name, (MAIN / name).read_text(),
                                                OUTBOX_RETENTION_UPDATES, OUTBOX_RETENTION_UPDATES_SHA,
                                                OUTBOX_RETENTION_FILES)
                current = reverse_reviewed_patch(name, current,
                                                SEND_RECEIPT_UPDATES, SEND_RECEIPT_UPDATES_SHA, SEND_RECEIPT_FILES)
                current = reverse_reviewed_patch(name, current,
                                                READABLE_VISIBILITY_UPDATES, READABLE_VISIBILITY_UPDATES_SHA,
                                                READABLE_VISIBILITY_FILES)
                current = reverse_reviewed_patch(name, current,
                                                CURRENT_READ_FLAGS_UPDATES, CURRENT_READ_FLAGS_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                CURRENT_READ_UPDATES, CURRENT_READ_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                QUESTION_PROJECTION_COMPLETE_UPDATES, QUESTION_PROJECTION_COMPLETE_UPDATES_SHA, QUESTION_PROJECTION_COMPLETE_FILES)
                current = reverse_reviewed_patch(name, current,
                                                QUESTION_PRESENCE_UPDATES, QUESTION_PRESENCE_UPDATES_SHA, QUESTION_PRESENCE_FILES)
                current = reverse_reviewed_patch(name, current,
                                                SYNC_STATUS_UPDATES, SYNC_STATUS_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                OUTBOX_RELEASE_REASON_UPDATES, OUTBOX_RELEASE_REASON_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                OUTGOING_SEND_UPDATES, OUTGOING_SEND_UPDATES_SHA, OUTGOING_SEND_FILES)
                current = reverse_reviewed_patch(name, current,
                                                SEND_ATTEMPT_UPDATES, SEND_ATTEMPT_UPDATES_SHA, SEND_ATTEMPT_FILES)
                current = reverse_reviewed_patch(name, current,
                                                PRESENTATION_UPDATES, PRESENTATION_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                DELIVERY_RECOVERY_UPDATES, DELIVERY_RECOVERY_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                RECENT_ACTIVITY_UPDATES, RECENT_ACTIVITY_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                CHILD_SESSION_UPDATES, CHILD_SESSION_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                VISIBLE_REFRESH_UPDATES, VISIBLE_REFRESH_UPDATES_SHA)
                current = reverse_reviewed_patch(name, current,
                                                TASK_TRACKER_UPDATES, TASK_TRACKER_UPDATES_SHA)
                projected = reverse_reviewed_patch(name, current,
                                                  WORKSPACE_UPDATES, WORKSPACE_UPDATES_SHA)
                self.assertEqual(hashlib.sha256(projected.encode()).hexdigest(), digest)

    def test_original_model_and_main_reconstruct_including_every_policy_and_assignment(self):
        for name, digest in [("ZerusViewModel.kt", MODEL_SHA), ("MainActivity.kt", MAIN_SHA)]:
            source = (MAIN / name).read_text()
            self.assertEqual(hashlib.sha256(reconstructed(name, source).encode()).hexdigest(), digest)
            changed = source.replace("activityVerified || model.demo", "true", 1) if name == "MainActivity.kt" else source.replace("it.blocksSending", "false", 1)
            self.assertFalse(original_source(name, changed))

    def test_reverse_updates_keep_every_question_policy_and_verification_assignment_protected(self):
        source = (MAIN / "ZerusViewModel.kt").read_text()
        for expression in (
                "sessionActions.any { it.target == target && it.blocksSending }",
                "clearExpectation?.let { ContextPolicies.sameSessionRun(it.target, target) } == true",
                "contextOperations.any { ContextPolicies.sameSessionRun(it.target, target) && it.blocksSending }"):
            with self.subTest(policy=expression):
                self.assertIn(expression, source)
                self.assertFalse(original_source("ZerusViewModel.kt", source.replace(expression, "false", 1)))
        writes = list(re.finditer(r"\bactivityVerified\s*=(?!=)\s*(false|verified|activeFrameVerified)(?= /\* BEGIN QUESTION_AVAILABILITY_PROBE writer-)", source))
        self.assertEqual(len(writes), len(WRITERS))
        for writer, match in zip(WRITERS, writes):
            with self.subTest(writer=writer):
                changed = source[:match.start(1)] + "true" + source[match.end(1):]
                self.assertFalse(original_source("ZerusViewModel.kt", changed))
        main = (MAIN / "MainActivity.kt").read_text()
        for expression in ("model.activityVerified || model.demo",
                           "online && model.activityVerified && !model.contextBlocked(target)"):
            self.assertIn(expression, main)
            self.assertFalse(original_source("MainActivity.kt", main.replace(expression, "true", 1)))

    def test_outbox_retention_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The #2118 retention delta stays an exact hunk: changing, removing or
        # duplicating it must break its own reversal and the reconstruction.
        # A whole hunk present twice must be refused too, not reversed once.
        addition = "    private fun retainOutbox() {\n"
        source = (MAIN / "ZerusViewModel.kt").read_text()
        self.assertEqual(source.count(addition), 1)
        self.assert_refuses_duplicated_hunk("ZerusViewModel.kt", OUTBOX_RETENTION_UPDATES, addition)
        for kind, changed in (
                ("changed", source.replace(addition, addition.rstrip("\n") + " // changed reviewed addition\n", 1)),
                ("missing", source.replace(addition, "", 1)),
                ("duplicated line", source.replace(addition, addition + addition, 1))):
            with self.subTest(kind=kind):
                with self.assertRaises(AssertionError):
                    reverse_reviewed_patch("ZerusViewModel.kt", changed,
                                           OUTBOX_RETENTION_UPDATES, OUTBOX_RETENTION_UPDATES_SHA, OUTBOX_RETENTION_FILES)
                self.assertFalse(original_source("ZerusViewModel.kt", changed))

    def test_send_receipt_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The #2087 send-receipt delta stays exact hunks: changing, removing or
        # duplicating one of its additions must break its own reversal, after
        # the newer #2118 delta, and the reconstruction. A whole hunk present
        # twice after that reversal must be refused too, not reversed once.
        for addition in (
                "    private fun captureSendIntent(target:Target,id:String,text:String,compactionId:String?=null):SendIntent? {\n",
                "        val frozen=if(original.attachments.isEmpty()) captureSendIntent(target,id,original.text,expectedCompactionId.takeIf { it.isNotBlank() }) else null\n"):
            source = (MAIN / "ZerusViewModel.kt").read_text()
            self.assertEqual(source.count(addition), 1)
            for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
                with self.subTest(addition=addition[:40], replacement=replacement[:40]):
                    changed = source.replace(addition, replacement, 1)
                    newer = reverse_reviewed_patch("ZerusViewModel.kt", changed,
                                                   OUTBOX_RETENTION_UPDATES, OUTBOX_RETENTION_UPDATES_SHA, OUTBOX_RETENTION_FILES)
                    with self.assertRaises(AssertionError):
                        reverse_reviewed_patch("ZerusViewModel.kt", newer,
                                               SEND_RECEIPT_UPDATES, SEND_RECEIPT_UPDATES_SHA, SEND_RECEIPT_FILES)
                    self.assertFalse(original_source("ZerusViewModel.kt", changed))
            self.assert_refuses_duplicated_hunk("ZerusViewModel.kt", SEND_RECEIPT_UPDATES, addition)

    def test_outbox_release_reason_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The release and local-reason delta stays exact: changing, removing or
        # duplicating either reviewed hunk must break the reconstruction.
        additions = {
            "ZerusViewModel.kt": "            release = { api.releaseReservedOutgoing(connection, id) })\n",
            "MainActivity.kt": "                else if (message.error.isNotBlank()) message.error\n",
        }
        for name, addition in additions.items():
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, OUTBOX_RELEASE_REASON_UPDATES, addition)
            for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
                with self.subTest(source=name, replacement=replacement):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_outgoing_send_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The send-entry wiring delta stays an exact hunk: changing, removing
        # or duplicating it must break the reconstruction, never be hidden.
        addition = "                submitOutgoing(api, connection, target, payload, id, evidence)\n"
        source = (MAIN / "ZerusViewModel.kt").read_text()
        self.assertEqual(source.count(addition), 1)
        self.assert_refuses_duplicated_hunk("ZerusViewModel.kt", OUTGOING_SEND_UPDATES, addition)
        for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
            with self.subTest(replacement=replacement):
                self.assertFalse(original_source("ZerusViewModel.kt", source.replace(addition, replacement, 1)))

    def test_send_attempt_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The reviewed evidence delta stays an exact hunk: changing, removing
        # or duplicating it must break the reconstruction, never be hidden. The
        # newer #2044 send-entry delta is reversed first, as in the chain, after
        # the still newer #2118 and #2087 deltas.
        addition = "        val attempt = SendAttempt(); var rejected: RelayException? = null\n"
        source = reverse_reviewed_patch("ZerusViewModel.kt", (MAIN / "ZerusViewModel.kt").read_text(),
                                        OUTBOX_RETENTION_UPDATES, OUTBOX_RETENTION_UPDATES_SHA, OUTBOX_RETENTION_FILES)
        source = reverse_reviewed_patch("ZerusViewModel.kt", source,
                                        SEND_RECEIPT_UPDATES, SEND_RECEIPT_UPDATES_SHA, SEND_RECEIPT_FILES)
        source = reverse_reviewed_patch("ZerusViewModel.kt", source,
                                        OUTBOX_RELEASE_REASON_UPDATES, OUTBOX_RELEASE_REASON_UPDATES_SHA)
        source = reverse_reviewed_patch("ZerusViewModel.kt", source,
                                        OUTGOING_SEND_UPDATES, OUTGOING_SEND_UPDATES_SHA, OUTGOING_SEND_FILES)
        self.assertEqual(source.count(addition), 1)
        self.assert_refuses_duplicated_hunk("ZerusViewModel.kt", SEND_ATTEMPT_UPDATES, addition)
        for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
            with self.subTest(replacement=replacement):
                with self.assertRaises(AssertionError):
                    reverse_reviewed_patch("ZerusViewModel.kt", source.replace(addition, replacement, 1),
                                           SEND_ATTEMPT_UPDATES, SEND_ATTEMPT_UPDATES_SHA, SEND_ATTEMPT_FILES)

    def test_readable_visibility_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The readable-visibility delta stays exact reviewed hunks in its one file:
        # changing, removing or duplicating one of its additions must break the reconstruction.
        for name, addition in (
                ("MainActivity.kt", "    val readableHeight = with(LocalDensity.current) { ConversationViewport.READABLE_HEIGHT_DP.dp.roundToPx() }\n"),
                ("MainActivity.kt", "                val ids=ConversationViewport.visibleEventIds(items,start,end,readableHeight,::displayedEventId)\n")):
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, READABLE_VISIBILITY_UPDATES, addition)
            for replacement in (addition.replace("\n", " // changed reviewed addition\n", 1), "", addition + addition):
                with self.subTest(source=name, replacement=replacement[:40]):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))
        # It never touches the view model.
        self.assertEqual(READABLE_VISIBILITY_FILES, ("MainActivity.kt",))

    def test_current_read_flags_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The flags mapping stays exact reviewed hunks in both files: changing,
        # removing or duplicating one of its additions must break the reconstruction.
        for name, addition in (
                ("ZerusViewModel.kt", "    fun visibleCurrent(target:Target,key:String,flags:ConversationViewport.CurrentFlags) {\n"),
                ("MainActivity.kt", "                model.visibleCurrent(target,viewport.current,viewport.flags)\n")):
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, CURRENT_READ_FLAGS_UPDATES, addition)
            for replacement in (addition.replace("\n", " // changed reviewed addition\n", 1), "", addition + addition):
                with self.subTest(source=name, replacement=replacement[:40]):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_current_read_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The current-read acknowledgement stays exact reviewed hunks in both files:
        # changing, removing or duplicating one of its additions must break the reconstruction.
        for name, addition in (
                ("ZerusViewModel.kt", "    private fun launchSharedRead(request:SharedReadRequest) {\n"),
                ("MainActivity.kt", "                model.visibleMessages(target, viewport.ids)\n")):
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, CURRENT_READ_UPDATES, addition)
            for replacement in (addition.replace("\n", " // changed reviewed addition\n", 1), "", addition + addition):
                with self.subTest(source=name, replacement=replacement[:40]):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_sync_status_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # The status strip stays exact reviewed hunks in both files: changing,
        # removing or duplicating one of its additions must break the reconstruction.
        for name, addition in (
                ("ZerusViewModel.kt", "    var conversationObservedAt by mutableStateOf<Long?>(null); private set\n"),
                ("MainActivity.kt", "    Column(Modifier.fillMaxSize().imePadding()) {\n        ConversationStatusStrip(syncFacts, refreshing = model.detailBusy && model.detailProgress,\n")):
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            for replacement in (addition.replace("\n", " // changed reviewed addition\n", 1), "", addition + addition):
                with self.subTest(source=name, replacement=replacement[:40]):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))
            self.assert_refuses_duplicated_hunk(name, SYNC_STATUS_UPDATES, addition.splitlines(keepends=True)[-1])

    def test_question_presence_projection_refuses_changed_missing_or_duplicate_reviewed_hunk(self):
        # The check-age projection stays one exact reviewed hunk: changing,
        # removing or duplicating it must break the reconstruction.
        addition = "    var questionCheckedAt by mutableStateOf(0L); private set\n"
        source = (MAIN / "ZerusViewModel.kt").read_text()
        self.assertEqual(source.count(addition), 1)
        self.assert_refuses_duplicated_hunk("ZerusViewModel.kt", QUESTION_PRESENCE_UPDATES, addition)
        for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
            with self.subTest(replacement=replacement):
                self.assertFalse(original_source("ZerusViewModel.kt", source.replace(addition, replacement, 1)))

    def test_question_projection_complete_projection_refuses_changed_missing_or_duplicate_reviewed_hunk(self):
        # The completeness projection stays an exact reviewed hunk: changing,
        # removing or duplicating it must break the reconstruction.
        addition = "    var questionProjectionComplete by mutableStateOf(true); private set\n"
        source = (MAIN / "ZerusViewModel.kt").read_text()
        self.assertEqual(source.count(addition), 1)
        self.assert_refuses_duplicated_hunk("ZerusViewModel.kt", QUESTION_PROJECTION_COMPLETE_UPDATES, addition)
        for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
            with self.subTest(replacement=replacement):
                self.assertFalse(original_source("ZerusViewModel.kt", source.replace(addition, replacement, 1)))

    def test_visible_refresh_projection_refuses_modified_missing_or_duplicate_hunks(self):
        # Reversing the exact UI change must not hide a later policy edit. The
        # visible-refresh status row itself became read timing in 288a92bf5 and is
        # covered by the presentation projection; this is the reviewed background
        # refresh gate that still yields to native question review.
        additions = {
            "ZerusViewModel.kt": "    private var lastHistoryReadCompletedAt=0L\n",
            "MainActivity.kt": "        if (refreshTarget?.archiveId?.isBlank() == true && !reviewingNativeInput && !model.demo) {\n",
        }
        for name, addition in additions.items():
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, VISIBLE_REFRESH_UPDATES, addition)
            for replacement in (addition.rstrip("\n") + " // changed\n", "", addition + addition):
                with self.subTest(source=name, replacement=replacement):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_recent_activity_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        additions = {
            "ZerusViewModel.kt": "    private var historyViewIntent=0L\n",
            "MainActivity.kt": "        val needsHistory = model.hasNewer || model.olderLoading\n",
        }
        for name, addition in additions.items():
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, RECENT_ACTIVITY_UPDATES, addition)
            for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
                with self.subTest(source=name, replacement=replacement):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_presentation_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        additions = {
            "ZerusViewModel.kt": "    internal val conversationSupersededIds:Map<String,String> get() = activeFrame?.supersededIds.orEmpty()\n",
            "MainActivity.kt": "            if (selected != null) ConversationReadTiming(model, conversationLoad)\n",
        }
        for name, addition in additions.items():
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, PRESENTATION_UPDATES, addition)
            for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
                with self.subTest(source=name, replacement=replacement):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_delivery_recovery_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        additions = {
            "ZerusViewModel.kt": "    fun dismissOutgoing(message: OutgoingMessage) = viewModelScope.launch {\n",
            "MainActivity.kt": "        val canDismiss = !message.deliveryDismissed && message.status in listOf(\"uncertain\", \"failed\")\n",
        }
        for name, addition in additions.items():
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, DELIVERY_RECOVERY_UPDATES, addition)
            for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
                with self.subTest(source=name, replacement=replacement):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_delivery_recovery_projection_keeps_local_intent_and_inflight_guards_protected(self):
        for name, expressions in {
            "ZerusViewModel.kt": (
                "if (!storageReady || flight in receiptFlights || message.requestId in deliveryFlights) return@launch",
                "MessageChanges.dismissOutgoing(state, message, recoverInterruptedSending = true)"),
            "MainActivity.kt": (
                "if (message.status == \"failed\") TextButton(enabled =",
                "model.dismissOutgoing(message); reviewOutgoing = null",
                "val previous = model.outgoing.lastOrNull { it.belongsTo(target) && it.blocksSending }"),
        }.items():
            source = (MAIN / name).read_text()
            for expression in expressions:
                with self.subTest(source=name, guard=expression):
                    self.assertEqual(source.count(expression), 1)
                    self.assertFalse(original_source(name, source.replace(expression, "false", 1)))

    def test_recent_activity_projection_keeps_fresh_inspection_and_publication_ownership_protected(self):
        source = (MAIN / "ZerusViewModel.kt").read_text()
        for expression in (
                "inspectOnce(connection,target,fresh=true,current=::current).await()",
                "check(inspected.target==target)",
                "installFrame(recent,true)",
                "fun ownsDetail() = detailGate.owns(operation.id) && navigationId == navigation &&\n            historyViewIntent==historyIntent",
                "if (detailGate.owns(operation.id) && navigationId == navigation && historyViewIntent==historyIntent"):
            self.assertEqual(source.count(expression), 1)
            with self.subTest(ownership=expression):
                self.assertFalse(original_source("ZerusViewModel.kt", source.replace(expression, "false", 1)))

    def test_child_session_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        # Reversing the exact child-session delta must not hide a later edit.
        additions = {
            "ZerusViewModel.kt": "    fun teamCollapsed(session: Session) = teamCollapse.collapsed(session, machineUuid(session.target))\n",
            "MainActivity.kt": "                            SessionTeamCounter(model, entry, online)\n",
        }
        for name, addition in additions.items():
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, CHILD_SESSION_UPDATES, addition)
            for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
                with self.subTest(source=name, replacement=replacement):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_task_tracker_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        additions = {
            "ZerusViewModel.kt": "    fun stopTaskSync() { taskSyncJob?.cancel(); projectSyncs.toList().forEach { it.cancel() } }\n",
            "MainActivity.kt": "            model.syncTasks()\n",
        }
        for name, addition in additions.items():
            source = (MAIN / name).read_text()
            self.assertEqual(source.count(addition), 1)
            self.assert_refuses_duplicated_hunk(name, TASK_TRACKER_UPDATES, addition)
            for replacement in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
                with self.subTest(source=name, replacement=replacement):
                    self.assertFalse(original_source(name, source.replace(addition, replacement, 1)))

    def test_reverse_updates_refuse_changed_missing_or_duplicate_reviewed_hunks(self):
        source = (MAIN / "ZerusViewModel.kt").read_text()
        # These are reviewed additions, not probe markers or protected old policy.
        for old, new in (("RepositoryAcquisition.statusPayload(stored,id)", "JSONObject()"),
                         ("val loaded=loadedReadRows()", "val loaded=emptyMap<Target,List<Event>>()"),
                         ('    fun openLaunch() {\n', ''),
                         ('    fun openLaunch() {\n', '    fun openLaunch() {\n    fun openLaunch() {\n')):
            with self.subTest(update=old, replacement=new):
                self.assertIn(old, source)
                self.assertFalse(original_source("ZerusViewModel.kt", source.replace(old, new, 1)))
        main = (MAIN / "MainActivity.kt").read_text()
        self.assertFalse(original_source("MainActivity.kt", main.replace("!SharedReadPolicies.canShowReplyBadge(session)", "true", 1)))
        for name, addition in (("ZerusViewModel.kt", "RepositoryAcquisition.statusPayload(stored,id)"),
                               ("ZerusViewModel.kt", "        val loaded=loadedReadRows()\n"),
                               ("ZerusViewModel.kt", "    fun openLaunch() {\n"),
                               ("MainActivity.kt", "!SharedReadPolicies.canShowReplyBadge(session)")):
            self.assert_refuses_duplicated_hunk(name, UPDATES, addition)

    def test_workspace_projection_refuses_changed_missing_or_duplicate_reviewed_hunks(self):
        model = (MAIN / "ZerusViewModel.kt").read_text()
        for addition in (
                "    private val api = RelayApi(workspace = store.workspaceJournal)\n",
                "    var pendingEncryptedPairings by mutableStateOf<Set<String>>(emptySet());private set\n",
                "    fun resumeEncryptedPair(connection:Connection)=viewModelScope.launch {\n",
                "                        AppTelemetry.encryptedWorkspaceReady(getApplication(),connection)\n",
                "                        WorkspaceCatalogCache.preserve(connection,computers,\n",
                "                sessions = preparedSessions; machines = newMachines.map { MachineNames.apply(it,messageState.machineAliases) }; machineOperations = newOperations; machineFeatures = newFeatures\n"):
            self.assert_refuses_duplicated_hunk("ZerusViewModel.kt", WORKSPACE_UPDATES, addition)
            for changed in (addition.rstrip("\n") + " // changed reviewed addition\n", "", addition + addition):
                with self.subTest(addition=addition, replacement=changed):
                    self.assertIn(addition, model)
                    self.assertNotEqual(addition, changed)
                    self.assertFalse(original_source("ZerusViewModel.kt", model.replace(addition, changed, 1)))
        main = (MAIN / "MainActivity.kt").read_text()
        for addition in ("Messages are encrypted between enrolled devices.",
                         "This erases this phone's connection keys."):
            self.assert_refuses_duplicated_hunk("MainActivity.kt", WORKSPACE_UPDATES, addition)
            for changed in ("Changed reviewed text.", "", addition + addition):
                with self.subTest(addition=addition, replacement=changed):
                    self.assertIn(addition, main)
                    self.assertFalse(original_source("MainActivity.kt", main.replace(addition, changed, 1)))

    def test_each_existing_write_is_followed_by_its_fixed_reason_without_changing_rhs(self):
        source = (MAIN / "ZerusViewModel.kt").read_text()
        writes = re.findall(r"\bactivityVerified\s*=(?!=)\s*(false|verified|activeFrameVerified) /\* BEGIN QUESTION_AVAILABILITY_PROBE (writer-[a-z-]+) \*/; questionVerificationProbe\(QuestionUiProbe.VerificationWriter\.([A-Z_]+)\) /\* END QUESTION_AVAILABILITY_PROBE \2 \*/", source)
        self.assertEqual([w[2] for w in writes], WRITERS)
        self.assertEqual([w[0] for w in writes], ["false", "verified", "activeFrameVerified"] + ["false"] * 7)
        self.assertEqual(len(re.findall(r"\bactivityVerified\s*=(?!=)", strip_availability(source))), 10)

    def test_availability_inputs_are_captured_before_side_effect_and_preserve_the_actual_expression(self):
        model = (MAIN / "ZerusViewModel.kt").read_text().split("internal fun questionAvailabilityProbe(", 1)[1].split("// END QUESTION_AVAILABILITY_PROBE definitions", 1)[0]
        self.assertLess(model.index("if (!QuestionUiProbe.enabled) return null"), model.index("val verified = activityVerified"))
        self.assertIn("val blocked = contextBlocked(target)", model)
        for term in ("sessionActions.any { it.target == target && it.blocksSending }", "clearExpectation?.let { ContextPolicies.sameSessionRun(it.target, target) } == true", "contextOperations.any { ContextPolicies.sameSessionRun(it.target, target) && it.blocksSending }"):
            self.assertIn(term, model)
        self.assertIn("online && verified && !blocked", model)
        main = (MAIN / "MainActivity.kt").read_text().split("// BEGIN QUESTION_AVAILABILITY_PROBE committed", 1)[1].split("// END QUESTION_AVAILABILITY_PROBE committed", 1)[0]
        self.assertLess(main.index("if (QuestionUiProbe.enabled)"), main.index("model.questionAvailabilityProbe(target, online)"))
        self.assertIn("SideEffect { QuestionUiProbe.availability(target, observedAvailability) }", main)
        self.assertNotIn("model.", main.split("SideEffect", 1)[1])

    def test_model_observation_is_default_null_bounded_typed_and_catches_only_observer_work(self):
        model = (MAIN / "ZerusViewModel.kt").read_text().split("private fun questionVerificationProbe(", 1)[1].split("internal fun questionAvailabilityProbe(", 1)[0]
        self.assertLess(model.index("if (!QuestionUiProbe.enabled) return"), model.index("try {"))
        self.assertLess(model.index("try {"), model.index("val frame = activeFrame"))
        self.assertIn("catch (_: Throwable) { QuestionUiProbe.observationFailed() }", model)
        probe = (MAIN / "QuestionUiProbe.kt").read_text()
        observer = probe.split("private inline fun observeTarget(", 1)[1].split("    fun verification(", 1)[0]
        guard = "if (!BuildConfig.DEBUG || !owned.healthy()) return"
        self.assertLess(observer.index("val owned = session ?: return"), observer.index(guard))
        for work in ("SystemClock.uptimeMillis()", "target?.let", "fingerprint(identity)", "Record(", "detail()"):
            self.assertLess(observer.index(guard), observer.index(work))
        self.assertIn("catch (_: Throwable) { owned.failed() }", observer)
        self.assertIn("const val MAX_RECORDS = 256", probe)
        self.assertIn("if (records.size == MAX_RECORDS) { fault = Fault.OVERFLOW; return }", probe)
        self.assertIn("record.stage == Stage.AVAILABILITY_COMMIT && same(availability, record)", probe)
        self.assertIn("data class Verification(val writer: VerificationWriter", probe)
        for forbidden in ("ZerusViewModel", "JSONObject", "question.prompts", "choice.label", "java.io", "okhttp", "Thread(", "launch(", "delay("):
            self.assertNotIn(forbidden, probe)

    def test_question_ui_and_entire_physical_fixture_match_the_reviewed_baselines(self):
        self.assertEqual(hashlib.sha256((MAIN / "QuestionUi.kt").read_bytes()).hexdigest(),
                         "53816da954ccfc33a8a64a72f5d5930b286f51b7586cc577bbcb4e2762317cb3")
        fixture = ROOT / "mobile/android/app/src/androidTest/java/app/zerus/mobile/ApprovalChoicesSmokeInstrumentation.kt"
        # The fixture baseline includes the parent-selection and Compose role-child corrections;
        # physical case oracles stay exact.
        self.assertEqual(hashlib.sha256(original_review_fixture().encode()).hexdigest(),
                         "73cee450f08e33c007ad6f3188a6a69135674e2503f80ea48582ba8aa394a9f7")

    def test_prepared_publication_uses_the_actual_bounded_owner_before_any_atomic_install(self):
        model = (MAIN / "ZerusViewModel.kt").read_text()
        normalized = model.split("private fun frameInstall(", 1)[1].split("private fun installFrame(", 1)[0]
        self.assertNotIn("currentHistoryHeads-=", normalized)
        self.assertNotRegex(normalized, r"historyWindows\[[^\n]+\]\s*=")
        body = model.split("val result = InspectionPublication.prepare(", 1)[1].split("if (result == InspectionPublication.Result.SUPERSEDED)", 1)[0]
        for guard in ("activeFrame === preparationOwner", "clearExpectation == clearOwner", "messageState === captured.state",
                      "presentationRevision == captured.revision", "historyWindows[frame.target.key] === captured.window",
                      "outgoing.filter { it.belongsTo(frame.target) } == captured.messages",
                      "selected === captured.selection", "catalogSessions === captured.catalog",
                      "historyComplete == captured.complete", "historyIndexing == captured.indexing"):
            self.assertIn(guard, body)
        self.assertIn("withContext(Dispatchers.Default)", body)
        self.assertIn("Snapshot.withMutableSnapshot", body)
        self.assertLess(body.index("captured.install.frame.presentation = prepared"), body.index("installFrame(frame, true, captured.install)"))
        self.assertIn("if (result == InspectionPublication.Result.SUPERSEDED) return@launch", model)
        self.assertIn("check(result == InspectionPublication.Result.PUBLISHED)", model)
        helper = (MAIN / "InspectionPublication.kt").read_text()
        self.assertIn("const val MAX_ATTEMPTS = 3", helper)
        self.assertIn("repeat(MAX_ATTEMPTS)", helper)
        self.assertEqual(helper.count("currentCoroutineContext().ensureActive()"), 2)
        self.assertLess(helper.index("if (!owns()) return Result.SUPERSEDED", helper.index("val prepared = build(snapshot)")), helper.index("publish(snapshot, prepared)"))
        for forbidden in ("delay(", "launch(", "async(", "api.", "installFrame(", "while ("):
            self.assertNotIn(forbidden, helper)


if __name__ == "__main__":
    unittest.main()
