"""Credential-free contracts for the winget manifest and its submission."""
import importlib.util
from pathlib import Path
import re
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import winget_manifest

spec = importlib.util.spec_from_file_location("publish_winget", ROOT / "scripts/publish-winget.py")
publish_winget = importlib.util.module_from_spec(spec)
spec.loader.exec_module(publish_winget)

HASH = "ab" * 32
ZIP = "zerus-0.44.0-windows-x86_64.zip"
PREFIX = "manifests/u/ufna/Zerus/0.44.0/"


class FakeGitHub:
    """Answers by (method, path); unknown requests fail the test."""

    def __init__(self, answers):
        self.answers = answers
        self.calls = []

    def request(self, method, path, body=None, raw=False, timeout=60):
        self.calls.append((method, path, body))
        key = (method, path) if (method, path) in self.answers else (method, path.split("?")[0])
        if key not in self.answers:
            raise AssertionError(f"unexpected request {key}")
        answer = self.answers[key]
        return answer(body) if callable(answer) else answer


CLA_URL = "https://github.com/microsoft/winget-pkgs/pull/1"
CLA_PULL = "repos/microsoft/winget-pkgs/pulls/1"
CLA_COMMENTS = "repos/microsoft/winget-pkgs/issues/1/comments"


def cla_pull(labels=("Needs-CLA",), author="ufna", state="open", head="4" * 40):
    return dict(state=state, user=dict(login=author), html_url=CLA_URL,
                labels=[dict(name=name) for name in labels],
                base=dict(ref="master", sha="1" * 40, repo=dict(full_name="microsoft/winget-pkgs")),
                head=dict(sha=head, ref="ufna.Zerus-0.44.0", repo=dict(full_name="ufna/winget-pkgs")))


def cla_answers(comments=None, pull=None):
    comments = [] if comments is None else comments

    def post(body):
        comment = dict(body=body["body"], user=dict(login="ufna"), html_url=CLA_URL + "#issuecomment-1")
        comments.append(comment)
        return 201, comment

    return {
        ("GET", CLA_PULL): (200, pull or cla_pull()),
        ("GET", CLA_PULL + "/files"): (200, [dict(filename=PREFIX + name, status="added") for name in
            ["ufna.Zerus.yaml", "ufna.Zerus.installer.yaml", "ufna.Zerus.locale.en-US.yaml"]]),
        ("GET", CLA_COMMENTS): (200, comments),
        ("POST", CLA_COMMENTS): post,
    }


def release(digest="sha256:" + HASH, draft=False, prerelease=False, assets=(ZIP, "SHA256SUMS")):
    return {"draft": draft, "prerelease": prerelease, "published_at": "2026-10-16T22:43:02Z",
            "assets": [{"name": name, "digest": digest if name == ZIP else "sha256:" + "cd" * 32,
                        "browser_download_url": f"https://github.com/ufna/zerus/releases/download/v0.44.0/{name}"}
                       for name in assets]}


def published(release_answer, sums=f"{HASH}  {ZIP}\n{'ef' * 32}  zerus-0.44.0.tar.gz\n"):
    return FakeGitHub({
        ("GET", "repos/ufna/zerus/releases/tags/v0.44.0"): release_answer,
        ("GET", "https://github.com/ufna/zerus/releases/download/v0.44.0/SHA256SUMS"): (200, sums.encode()),
    })


def fork_answers(overrides=None):
    answers = {
        ("GET", "user"): (200, {"login": "ufna"}),
        ("GET", "repos/ufna/winget-pkgs"): (200, {"fork": True, "parent": {"full_name": "microsoft/winget-pkgs"}}),
        ("GET", "repos/microsoft/winget-pkgs/contents/" + PREFIX.rstrip("/")): (404, None),
        ("GET", "repos/microsoft/winget-pkgs/pulls"): (200, []),
        ("GET", "search/issues"): (200, {"items": []}),
        ("POST", "repos/ufna/winget-pkgs/merge-upstream"): (200, {}),
        ("GET", "repos/ufna/winget-pkgs/git/ref/heads/master"): (200, {"object": {"sha": "1" * 40}}),
        ("GET", "repos/ufna/winget-pkgs/git/commits/" + "1" * 40): (200, {"tree": {"sha": "2" * 40}}),
        ("POST", "repos/ufna/winget-pkgs/git/trees"): (201, {"sha": "3" * 40}),
        ("POST", "repos/ufna/winget-pkgs/git/commits"): (201, {"sha": "4" * 40}),
        ("POST", "repos/ufna/winget-pkgs/git/refs"): (201, {}),
        ("GET", "repos/microsoft/winget-pkgs/contents/manifests/u/ufna/Zerus"): (404, None),
        ("POST", "repos/microsoft/winget-pkgs/pulls"): (201, {"html_url": "https://github.com/microsoft/winget-pkgs/pull/1"}),
    }
    answers.update(cla_answers())
    answers.update(overrides or {})
    return answers


class WingetManifestTests(unittest.TestCase):
    def test_manifests_name_the_immutable_release_zip(self):
        files = winget_manifest.manifests("0.44.0", HASH, "2026-10-16")
        self.assertEqual(sorted(files), [PREFIX + "ufna.Zerus.installer.yaml", PREFIX + "ufna.Zerus.locale.en-US.yaml",
                                         PREFIX + "ufna.Zerus.yaml"])
        installer = files[PREFIX + "ufna.Zerus.installer.yaml"]
        self.assertIn("  InstallerUrl: https://github.com/ufna/zerus/releases/download/v0.44.0/" + ZIP + "\n", installer)
        self.assertIn("  InstallerSha256: " + HASH.upper() + "\n", installer)
        self.assertIn("InstallerType: zip\nNestedInstallerType: portable\n", installer)
        # The executable loads Qt from its own folder, so winget must put that folder on PATH.
        self.assertIn("- RelativeFilePath: zerus-windows-x86_64/hgs-tray.exe\n", installer)
        self.assertIn("ArchiveBinariesDependOnPath: true\n", installer)
        self.assertIn("- Architecture: x64\n", installer)
        self.assertIn("ReleaseDate: 2026-10-16\n", installer)
        for path, text in files.items():
            with self.subTest(path=path):
                kind = {"ufna.Zerus.yaml": "version", "ufna.Zerus.installer.yaml": "installer"}.get(
                    Path(path).name, "defaultLocale")
                self.assertTrue(text.startswith("# Created by scripts/publish-winget.py"))
                self.assertIn(f"$schema=https://aka.ms/winget-manifest.{kind}.1.12.0.schema.json\n", text)
                self.assertIn("PackageIdentifier: ufna.Zerus\nPackageVersion: 0.44.0\n", text)
                self.assertIn(f"ManifestType: {kind}\n", text)
                self.assertTrue(text.endswith("ManifestVersion: 1.12.0\n"))
                self.assertNotIn("\t", text)
                self.assertNotIn("nightly", text.lower())
        locale = files[PREFIX + "ufna.Zerus.locale.en-US.yaml"]
        self.assertIn("License: MIT\n", locale)
        self.assertIn("ReleaseNotesUrl: https://github.com/ufna/zerus/releases/tag/v0.44.0\n", locale)
        self.assertRegex(locale, r"(?m)^ShortDescription: [^:\n]{1,256}$")

    def test_only_stable_versions_with_a_windows_zip(self):
        for version in ("0.42.2", "0.36.0", "0.44.0-dev.5", "0.44.0.r1.gaaaaaaa.n2", "v0.44.0", ""):
            with self.subTest(version=version), self.assertRaises(ValueError):
                winget_manifest.manifests(version, HASH, "2026-10-16")
        for sha, date in (("AB" * 32, "2026-10-16"), (HASH[:-1], "2026-10-16"), (HASH, "16.10.2026")):
            with self.subTest(sha=sha, date=date), self.assertRaises(ValueError):
                winget_manifest.manifests("0.44.0", sha, date)
        self.assertIn(PREFIX.replace("0.44.0", "0.42.3") + "ufna.Zerus.yaml",
                      winget_manifest.manifests("0.42.3", HASH, "2026-10-14"))

    def test_webview2_is_an_external_prerequisite_and_the_portable_layout_is_unchanged(self):
        files = winget_manifest.manifests("0.44.0", HASH, "2026-10-16")
        installer = files[PREFIX + "ufna.Zerus.installer.yaml"]
        self.assertIn("Dependencies:\n  PackageDependencies:\n  - PackageIdentifier: Microsoft.EdgeWebView2Runtime\n", installer)
        self.assertIn("- RelativeFilePath: zerus-windows-x86_64/hgs-tray.exe\n", installer)
        self.assertIn("ArchiveBinariesDependOnPath: true\n", installer)


class PublishedZipTests(unittest.TestCase):
    def test_hash_must_match_sums_and_github_digest(self):
        self.assertEqual(publish_winget.published_zip(published((200, release())), "0.44.0"), (HASH, "2026-10-16"))
        failures = {
            "digest": published((200, release(digest="sha256:" + "00" * 32))),
            "no digest": published((200, release(digest=None))),
            "draft": published((200, release(draft=True))),
            "prerelease": published((200, release(prerelease=True))),
            "missing zip": published((200, release(assets=("SHA256SUMS",)))),
            "unpublished": published((404, None)),
            "ambiguous sums": published((200, release()), sums=f"{HASH}  {ZIP}\n{HASH}  {ZIP}\n"),
            "absent from sums": published((200, release()), sums=f"{HASH}  other.zip\n"),
        }
        for name, github in failures.items():
            with self.subTest(name), self.assertRaises(ValueError):
                publish_winget.published_zip(github, "0.44.0")


class SubmitTests(unittest.TestCase):
    files = winget_manifest.manifests("0.44.0", HASH, "2026-10-16")

    def test_new_package_is_one_commit_on_a_synchronized_fork(self):
        github = FakeGitHub(fork_answers())
        url = publish_winget.submit(github, "0.44.0", self.files, "https://github.com/ufna/zerus/actions/runs/7")
        self.assertEqual(url, "https://github.com/microsoft/winget-pkgs/pull/1")
        methods = [(method, path.split("?")[0]) for method, path, _ in github.calls]
        self.assertLess(methods.index(("POST", "repos/ufna/winget-pkgs/merge-upstream")),
                        methods.index(("GET", "repos/ufna/winget-pkgs/git/ref/heads/master")))
        body = {path.split("?")[0]: body for method, path, body in github.calls if method == "POST"}
        tree = body["repos/ufna/winget-pkgs/git/trees"]
        self.assertEqual(tree["base_tree"], "2" * 40)
        self.assertEqual({item["path"]: item["content"] for item in tree["tree"]}, self.files)
        self.assertEqual(body["repos/ufna/winget-pkgs/git/commits"]["parents"], ["1" * 40])
        self.assertEqual(body["repos/ufna/winget-pkgs/git/refs"], {"ref": "refs/heads/ufna.Zerus-0.44.0", "sha": "4" * 40})
        pull = body["repos/microsoft/winget-pkgs/pulls"]
        self.assertEqual(pull["title"], "New package: ufna.Zerus version 0.44.0")
        self.assertEqual((pull["head"], pull["base"]), ("ufna:ufna.Zerus-0.44.0", "master"))
        self.assertIn("actions/runs/7", pull["body"])
        # Personal CLA consent is an author reply, never an invented validation checkbox.
        self.assertNotRegex(pull["body"], r"\[x\] (?:Signed|Validated|Tested)")
        replies = [body for method, path, body in github.calls if (method, path) == ("POST", CLA_COMMENTS)]
        self.assertEqual(replies, [{"body": "@microsoft-github-policy-service agree"}])
        self.assertFalse(any(method in ("PUT", "PATCH", "DELETE") for method, _ in methods))

    def test_existing_package_gets_an_update_title(self):
        github = FakeGitHub(fork_answers({
            ("GET", "repos/microsoft/winget-pkgs/contents/manifests/u/ufna/Zerus"): (200, [])}))
        publish_winget.submit(github, "0.44.0", self.files)
        pull = next(body for method, path, body in github.calls if (method, path) == ("POST", "repos/microsoft/winget-pkgs/pulls"))
        self.assertEqual(pull["title"], "Update: ufna.Zerus to 0.44.0")

    def test_a_version_is_never_proposed_twice(self):
        cases = {
            "merged upstream": {("GET", "repos/microsoft/winget-pkgs/contents/" + PREFIX.rstrip("/")): (200, [])},
            "open pull": {("GET", "repos/microsoft/winget-pkgs/pulls"): (200, [{"state": "open", "html_url": "u/2"}])},
            "merged pull": {("GET", "repos/microsoft/winget-pkgs/pulls"): (200, [{"state": "closed", "merged_at": "x", "html_url": "u/3"}])},
            "another submitter": {("GET", "search/issues"): (200, {"items": [{"title": "New package: ufna.Zerus version 0.44.0", "html_url": "u/4"}]})},
        }
        for name, overrides in cases.items():
            with self.subTest(name):
                github = FakeGitHub(fork_answers(overrides))
                self.assertTrue(publish_winget.submit(github, "0.44.0", self.files))
                self.assertFalse(any(method == "POST" for method, _, _ in github.calls))
        # A similar version in another title is not this proposal.
        github = FakeGitHub(fork_answers({("GET", "search/issues"): (200, {"items": [{"title": "Update: ufna.Zerus to 0.44.01", "html_url": "u/5"}]})}))
        self.assertEqual(publish_winget.submit(github, "0.44.0", self.files), "https://github.com/microsoft/winget-pkgs/pull/1")

    def test_a_closed_proposal_or_foreign_fork_needs_a_person(self):
        cases = {
            "closed pull": {("GET", "repos/microsoft/winget-pkgs/pulls"): (200, [{"state": "closed", "merged_at": None, "html_url": "u/6"}])},
            "not a fork": {("GET", "repos/ufna/winget-pkgs"): (200, {"fork": False})},
            "missing fork": {("GET", "repos/ufna/winget-pkgs"): (404, None)},
            "diverged fork": {("POST", "repos/ufna/winget-pkgs/merge-upstream"): (422, None)},
        }
        for name, overrides in cases.items():
            with self.subTest(name), self.assertRaises(ValueError):
                publish_winget.submit(FakeGitHub(fork_answers(overrides)), "0.44.0", self.files)

    def test_an_interrupted_branch_is_moved_to_the_new_commit(self):
        github = FakeGitHub(fork_answers({
            ("POST", "repos/ufna/winget-pkgs/git/refs"): (422, None),
            ("PATCH", "repos/ufna/winget-pkgs/git/refs/heads/ufna.Zerus-0.44.0"): (200, {})}))
        publish_winget.submit(github, "0.44.0", self.files)
        patch = next(body for method, path, body in github.calls if method == "PATCH")
        self.assertEqual(patch, {"sha": "4" * 40, "force": True})


class ClaConsentTests(unittest.TestCase):
    def confirm(self, github, **kwargs):
        publish_winget.confirm_cla(github, "ufna", CLA_URL, "0.44.0", **kwargs)

    def test_personal_consent_is_posted_once_even_when_existing_proposal_is_retried(self):
        github = FakeGitHub(fork_answers({("GET", "repos/microsoft/winget-pkgs/pulls"):
            (200, [dict(state="open", html_url=CLA_URL)])}))
        for _ in range(2):
            self.assertEqual(publish_winget.submit(github, "0.44.0", SubmitTests.files), CLA_URL)
        writes = [(path, body) for method, path, body in github.calls if method == "POST"]
        self.assertEqual(writes, [(CLA_COMMENTS, {"body": publish_winget.CLA_REPLY})])

    def test_other_accounts_repositories_and_authors_are_not_signed_for(self):
        for login, url in [("someone", CLA_URL), ("ufna", "https://github.com/example/repository/pull/1")]:
            github = FakeGitHub({})
            publish_winget.confirm_cla(github, login, url, "0.44.0")
            self.assertEqual(github.calls, [])
        github = FakeGitHub(cla_answers(pull=cla_pull(author="someone")))
        self.confirm(github)
        self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_closed_proposals_do_not_get_comments(self):
        github = FakeGitHub(cla_answers(pull=cla_pull(state="closed")))
        self.confirm(github)
        self.assertEqual(len(github.calls), 1)

    def test_foreign_fork_branch_and_unrelated_manifest_files_refuse_before_writing(self):
        for field, value in [("repo", dict(full_name="someone/winget-pkgs")), ("ref", "unrelated")]:
            pull = cla_pull(); pull["head"][field] = value
            github = FakeGitHub(cla_answers(pull=pull))
            with self.assertRaises(ValueError): self.confirm(github)
            self.assertFalse(any(method == "POST" for method, _, _ in github.calls))
        answers = cla_answers(); answers[("GET", CLA_PULL + "/files")] = (200, [dict(filename="unrelated.yaml")])
        github = FakeGitHub(answers)
        with self.assertRaises(ValueError): self.confirm(github)
        self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_an_author_reply_on_later_comment_pages_is_not_duplicated(self):
        answers = cla_answers()
        answers[("GET", CLA_COMMENTS + "?per_page=100&page=1")] = (200,
            [dict(user=dict(login="another-user"), body=publish_winget.CLA_REPLY)] * 100)
        answers[("GET", CLA_COMMENTS + "?per_page=100&page=2")] = (200,
            [dict(user=dict(login="ufna"), body="Already authorized: " + publish_winget.CLA_REPLY)])
        github = FakeGitHub(answers); self.confirm(github)
        self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_expected_destinations_renamed_from_another_package_are_not_confirmed(self):
        answers = cla_answers()
        files = answers[("GET", CLA_PULL + "/files")][1]
        for item in files:
            item.update(status="renamed", previous_filename="manifests/e/Example/Other/1.0.0/" + Path(item["filename"]).name)
        github = FakeGitHub(answers)
        with self.assertRaises(ValueError): self.confirm(github)
        self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_same_head_with_a_retargeted_base_is_refused_during_wait_and_final_admission(self):
        for phase in ("wait", "final"):
            with self.subTest(phase=phase):
                answers = cla_answers(); reads = []
                def read(_):
                    reads.append(1)
                    pull = cla_pull(labels=() if phase == "wait" and len(reads) == 1 else ("Needs-CLA",))
                    if len(reads) >= 2: pull["base"]["ref"] = "different-base"
                    return 200, pull
                answers[("GET", CLA_PULL)] = read; github = FakeGitHub(answers)
                with patch.object(publish_winget.time, "sleep"), self.assertRaises(ValueError):
                    self.confirm(github, wait_for_prompt=True)
                self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_an_agreement_arriving_during_the_final_read_is_not_duplicated(self):
        answers = cla_answers(); reads = []
        def comments(_):
            reads.append(1)
            return 200, ([] if len(reads) == 1 else [dict(user=dict(login="ufna"), body=publish_winget.CLA_REPLY)])
        answers[("GET", CLA_COMMENTS)] = comments
        github = FakeGitHub(answers); self.confirm(github)
        self.assertEqual(len(reads), 2)
        self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_unknown_comment_write_is_not_retried(self):
        answers = cla_answers(); answers[("POST", CLA_COMMENTS)] = (422, None)
        github = FakeGitHub(answers)
        with self.assertRaises(ValueError): self.confirm(github)
        self.assertEqual(sum(method == "POST" for method, _, _ in github.calls), 1)

    def test_incomplete_comment_history_never_guesses_that_consent_is_missing(self):
        answers = cla_answers()
        answers[("GET", CLA_COMMENTS)] = (200, [dict(user=dict(login="another-user"), body="other")] * 100)
        github = FakeGitHub(answers)
        with self.assertRaises(ValueError): self.confirm(github)
        self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_a_delayed_label_is_waited_for_without_real_sleep(self):
        answers = cla_answers(); reads = []
        def read(_):
            reads.append(1)
            return 200, cla_pull(labels=() if len(reads) <= 2 else ("Needs-CLA",))
        answers[("GET", CLA_PULL)] = read; github = FakeGitHub(answers)
        now = [0]
        with patch.object(publish_winget.time, "monotonic", side_effect=lambda: now[0]), \
                patch.object(publish_winget.time, "sleep", side_effect=lambda seconds: now.__setitem__(0, now[0] + seconds)):
            self.confirm(github, wait_for_prompt=True)
        self.assertEqual(now[0], 10)
        self.assertEqual(sum(method == "POST" for method, _, _ in github.calls), 1)

    def test_a_missing_prompt_has_a_finite_wait_and_no_comment(self):
        github = FakeGitHub(cla_answers(pull=cla_pull(labels=())))
        now = [0]
        with patch.object(publish_winget.time, "monotonic", side_effect=lambda: now[0]), \
                patch.object(publish_winget.time, "sleep", side_effect=lambda seconds: now.__setitem__(0, now[0] + seconds)):
            self.confirm(github, wait_for_prompt=True)
        self.assertEqual(now[0], 60)
        self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_a_head_changed_while_waiting_is_not_confirmed(self):
        answers = cla_answers(); reads = []
        def read(_):
            reads.append(1)
            return 200, cla_pull(labels=() if len(reads) == 1 else ("Needs-CLA",), head="4" * 40 if len(reads) == 1 else "5" * 40)
        answers[("GET", CLA_PULL)] = read; github = FakeGitHub(answers)
        with patch.object(publish_winget.time, "sleep"), self.assertRaises(ValueError):
            self.confirm(github, wait_for_prompt=True)
        self.assertFalse(any(method == "POST" for method, _, _ in github.calls))

    def test_scheduled_reconciliation_uses_the_same_guarded_reply_without_opening_a_pr(self):
        answers = cla_answers()
        answers[("GET", "user")] = (200, dict(login="ufna"))
        answers[("GET", "search/issues")] = (200, dict(total_count=1, incomplete_results=False, items=[dict(number=1)]))
        github = FakeGitHub(answers); publish_winget.confirm_pending(github)
        writes = [(path, body) for method, path, body in github.calls if method != "GET"]
        self.assertEqual(writes, [(CLA_COMMENTS, {"body": publish_winget.CLA_REPLY})])

    def test_scheduled_reconciliation_refuses_foreign_identity_and_truncated_search(self):
        for result in [dict(total_count=101, items=[]), dict(total_count=1, items=[]),
                       dict(total_count=0, items=[], incomplete_results=True)]:
            answers = {("GET", "user"): (200, dict(login="ufna")), ("GET", "search/issues"): (200, result)}
            github = FakeGitHub(answers)
            with self.assertRaises(ValueError): publish_winget.confirm_pending(github)
            self.assertFalse(any(method == "POST" for method, _, _ in github.calls))
        github = FakeGitHub({("GET", "user"): (200, dict(login="someone"))})
        with self.assertRaises(ValueError): publish_winget.confirm_pending(github)
        self.assertEqual(len(github.calls), 1)


class WorkflowTests(unittest.TestCase):
    def test_winget_token_is_confined_to_a_job_after_publication(self):
        for workflow in (ROOT / ".github/workflows").glob("*.yml"):
            if "WINGET_TOKEN" in workflow.read_text():
                self.assertIn(workflow.name, ["publish.yml", "winget-cla.yml"])
        text = (ROOT / ".github/workflows/publish.yml").read_text()
        job = re.search(r"(?ms)^  winget:\n(.*?)(?=^  \S|\Z)", text)[1]
        self.assertIn("    needs: publish\n", job)
        self.assertIn("    environment: release\n", job)
        self.assertIn("      group: winget-cla-consent\n", job)
        self.assertIn("      contents: read\n", job)
        self.assertNotRegex(job, r"(?m)^\s*(?:actions|contents): write")
        self.assertIn("persist-credentials: false", job)
        self.assertIn("test \"$WORKFLOW_REF\" = refs/heads/main", job)
        self.assertIn("python3 -m unittest discover -s tests -p test_winget_publication.py", job)
        self.assertIn("python3 scripts/publish-winget.py --version \"$RELEASE_VERSION\"", job)
        self.assertEqual(text.count("secrets.WINGET_TOKEN"), 1)
        publish = re.search(r"(?ms)^  publish:\n(.*?)(?=^  \S|\Z)", text)[1]
        self.assertNotIn("WINGET", publish)

    def test_delayed_confirmation_is_trusted_main_only_and_shares_publication_concurrency(self):
        text = (ROOT / ".github/workflows/winget-cla.yml").read_text()
        self.assertIn("  schedule:\n", text)
        self.assertIn("  workflow_dispatch:\n", text)
        self.assertNotIn("pull_request", text)
        self.assertIn("if: github.repository == 'ufna/zerus' && github.ref == 'refs/heads/main'", text)
        self.assertIn("    environment: release\n", text)
        self.assertIn("  group: winget-cla-consent\n", text)
        self.assertIn("persist-credentials: false", text)
        self.assertIn("python3 scripts/publish-winget.py --confirm-pending", text)
        self.assertEqual(text.count("secrets.WINGET_TOKEN"), 1)
        self.assertNotIn("contents: write", text)


if __name__ == "__main__":
    unittest.main()
