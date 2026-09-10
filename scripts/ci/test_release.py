"""Tests for the version gate that precedes registry publication."""

import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "check_release", Path(__file__).with_name("check-release.py")
)
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "src-python").mkdir()
        (self.root / "data/core").mkdir(parents=True)
        (self.root / "data/enhancement").mkdir()
        (self.root / "data/underwater-model-a").mkdir()
        (self.root / "data/underwater-model-b").mkdir()
        (self.root / "data/underwater-resources").mkdir()
        self.manifests = [
            "Cargo.toml", "data/core/Cargo.toml", "data/enhancement/Cargo.toml",
            "data/underwater-model-a/Cargo.toml",
            "data/underwater-model-b/Cargo.toml",
            "data/underwater-resources/Cargo.toml",
            "src-python/Cargo.toml",
        ]
        for relative in self.manifests:
            (self.root / relative).write_text('[package]\nversion.workspace = true\n')
        with (self.root / "Cargo.toml").open("a") as manifest:
            manifest.write('[workspace.package]\nversion = "1.2.3"\n[workspace.dependencies]\n')
            for name, path in {"insta360-rs": ".", **release.DATA_CRATES}.items():
                manifest.write(f'{name} = {{ version = "=1.2.3", path = "{path}" }}\n')
            manifest.write('[dependencies]\n')
            for name in release.DATA_CRATES:
                manifest.write(f'{name}.workspace = true\n')
        with (self.root / "src-python/Cargo.toml").open("a") as manifest:
            manifest.write('publish = false\n[dependencies]\ninsta360-rs.workspace = true\n')
        (self.root / "src-python/pyproject.toml").write_text(
            '[project]\ndynamic = ["version", "authors"]\n'
        )

    def test_matching_versions_are_accepted(self):
        self.assertEqual(release.validate("v1.2.3", self.root), "1.2.3")

    def test_workspace_version_must_match_tag(self):
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('version = "1.2.3"', 'version = "1.2.4"'))
        with self.assertRaisesRegex(ValueError, "found 1.2.4"):
            release.validate("v1.2.3", self.root)

    def test_each_manifest_must_inherit_version(self):
        for relative in self.manifests:
            with self.subTest(manifest=relative):
                manifest = self.root / relative
                original = manifest.read_text()
                manifest.write_text(original.replace('version.workspace = true', 'version = "1.2.3"'))
                with self.assertRaisesRegex(ValueError, "must inherit workspace.package.version"):
                    release.validate("v1.2.3", self.root)
                manifest.write_text(original)

    def test_python_version_must_be_dynamic(self):
        manifest = self.root / "src-python/pyproject.toml"
        for content in ['version = "1.2.3"', 'dynamic = ["authors"]', 'version = "1.2.3"\ndynamic = ["version"]']:
            with self.subTest(content=content):
                manifest.write_text('[project]\n' + content + '\n')
                with self.assertRaisesRegex(ValueError, "derive its version dynamically"):
                    release.validate("v1.2.3", self.root)

    def test_python_crate_cannot_be_published_to_crates_io(self):
        manifest = self.root / "src-python/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('publish = false', 'publish = true'))
        with self.assertRaisesRegex(ValueError, "publish = false"):
            release.validate("v1.2.3", self.root)

    def test_each_data_dependency_must_be_exactly_pinned(self):
        manifest = self.root / "Cargo.toml"
        original = manifest.read_text()
        for name in ["insta360-rs", *release.DATA_CRATES]:
            for version in ["1.2.3", "^1.2.3", "=1.2.4"]:
                with self.subTest(dependency=name, version=version):
                    manifest.write_text(original.replace(
                        f'{name} = {{ version = "=1.2.3"',
                        f'{name} = {{ version = "{version}"',
                    ))
                    with self.assertRaisesRegex(ValueError, "must pin"):
                        release.validate("v1.2.3", self.root)
        manifest.write_text(original)

    def test_each_data_dependency_must_use_the_expected_local_path(self):
        manifest = self.root / "Cargo.toml"
        original = manifest.read_text()
        for name, path in {"insta360-rs": ".", **release.DATA_CRATES}.items():
            with self.subTest(dependency=name):
                manifest.write_text(original.replace(f'path = "{path}"', 'path = "elsewhere"'))
                with self.assertRaisesRegex(ValueError, "must resolve"):
                    release.validate("v1.2.3", self.root)
        manifest.write_text(original)

    def test_dependencies_cannot_override_workspace_pins(self):
        for relative, names in [("Cargo.toml", release.DATA_CRATES), ("src-python/Cargo.toml", ["insta360-rs"])]:
            manifest = self.root / relative
            original = manifest.read_text()
            for name in names:
                with self.subTest(manifest=relative, dependency=name):
                    manifest.write_text(original.replace(
                        f'{name}.workspace = true', f'{name} = {{ version = "1.2.3" }}'
                    ))
                    with self.assertRaisesRegex(ValueError, "must inherit workspace dependency"):
                        release.validate("v1.2.3", self.root)
            manifest.write_text(original)

    def test_invalid_tags_never_reach_publication(self):
        for tag in ["", "1.2.3", "v1.2", "v01.2.3", "v1.2.3rc1", "v1.2.3-rc.1", "v1.2.3+build"]:
            with self.subTest(tag=tag):
                with self.assertRaisesRegex(ValueError, "vMAJOR.MINOR.PATCH"):
                    release.validate(tag, self.root)

    def git(self, *args):
        return subprocess.run(
            [
                "git", "-c", "user.name=Release tests", "-c", "user.email=tests@example.test",
                "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false",
                "-c", f"core.hooksPath={self.root / 'empty-hooks'}", *args,
            ],
            cwd=self.root, check=True, capture_output=True, text=True, timeout=10,
        ).stdout.strip()

    def initialize_git(self):
        self.git("init", "-b", "master")
        self.git("add", ".")
        self.git("commit", "-m", "Initial release")

    def test_matching_tag_accepts_lightweight_and_annotated_tags(self):
        self.initialize_git()
        for args in [("v1.2.3",), ("-a", "v1.2.3", "-m", "Release 1.2.3")]:
            with self.subTest(args=args):
                self.git("tag", *args)
                self.assertEqual(release.matching_tag(self.root), "v1.2.3")
                self.git("tag", "-d", "v1.2.3")

    def test_missing_or_different_version_tag_does_not_release(self):
        self.initialize_git()
        self.assertIsNone(release.matching_tag(self.root))
        self.git("tag", "v9.9.9")
        self.git("tag", "v1.2.3-rc.1")
        self.assertIsNone(release.matching_tag(self.root))
        self.git("tag", "v1.2.3")
        self.assertEqual(release.matching_tag(self.root), "v1.2.3")

    def test_matching_version_on_another_commit_does_not_release(self):
        self.initialize_git()
        self.git("tag", "-a", "v1.2.3", "-m", "Original release")
        self.git("commit", "--allow-empty", "-m", "Another commit")
        self.assertIsNone(release.matching_tag(self.root))

    def test_matching_branch_name_cannot_substitute_for_a_tag(self):
        self.initialize_git()
        self.git("branch", "v1.2.3")
        self.assertIsNone(release.matching_tag(self.root))

    def test_matching_tag_validates_pins_even_before_tag_exists(self):
        self.initialize_git()
        manifest = self.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace('version = "=1.2.3"', 'version = "=1.2.4"'))
        with self.assertRaisesRegex(ValueError, "must pin"):
            release.matching_tag(self.root)

    def test_matching_tag_does_not_hide_git_failures(self):
        with self.assertRaises(subprocess.CalledProcessError):
            release.matching_tag(self.root)

    def test_matching_tag_cli_stdout_is_only_the_tag_or_empty(self):
        self.initialize_git()
        for expected in ("", "v1.2.3\n"):
            with self.subTest(expected=expected):
                if expected:
                    self.git("tag", "v1.2.3")
                with mock.patch("sys.stdout", new_callable=io.StringIO) as stdout:
                    self.assertEqual(release.main(["--matching-tag"], self.root), 0)
                self.assertEqual(stdout.getvalue(), expected)

    def test_default_and_positional_tag_cli_behavior_is_retained(self):
        with mock.patch.dict(os.environ, {"GITHUB_REF_NAME": "v1.2.3"}, clear=True):
            for args in [[], ["v1.2.3"]]:
                with self.subTest(args=args):
                    with mock.patch("sys.stdout", new_callable=io.StringIO) as stdout:
                        self.assertEqual(release.main(args, self.root), 0)
                    self.assertIn("Validated release 1.2.3 for crates.io and PyPI", stdout.getvalue())

    def test_ci_gate_requires_tag_ref_and_validates_recorded_tag(self):
        environment = {
            "GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": "v1.2.3",
            "GITHUB_REPOSITORY": "OpenSpeleo/insta360-rs",
        }
        for field, value, error in [
            ("GITHUB_REF_TYPE", "branch", "tag ref"),
            ("GITHUB_REF_NAME", "v1.2.4", "found 1.2.3"),
            ("GITHUB_REPOSITORY", "", "GITHUB_REPOSITORY"),
            ("GITHUB_REPOSITORY", "owner/repo/elsewhere", "GITHUB_REPOSITORY"),
        ]:
            with self.subTest(field=field, value=value):
                with mock.patch.dict(os.environ, {**environment, field: value}, clear=True):
                    with mock.patch.object(release, "wait_for_ci") as wait:
                        with self.assertRaisesRegex(ValueError, error):
                            release.verify_ci(self.root, 123, 1)
                        wait.assert_not_called()

    def test_ci_cli_uses_checkout_commit_and_environment_tag(self):
        self.initialize_git()
        environment = {
            "GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": "v1.2.3",
            "GITHUB_REPOSITORY": "OpenSpeleo/insta360-rs",
        }
        with mock.patch.dict(os.environ, environment, clear=True):
            with mock.patch.object(release, "wait_for_ci") as wait:
                with mock.patch("sys.stdout", new_callable=io.StringIO) as stdout:
                    self.assertEqual(release.main(
                        ["--ci-run-id", "123", "--ci-run-attempt", "2"], self.root,
                    ), 0)
                wait.assert_called_once_with(
                    "OpenSpeleo/insta360-rs", 123, 2, self.git("rev-parse", "HEAD"),
                )
                self.assertIn("Validated release 1.2.3 against CI run 123, attempt 2", stdout.getvalue())

    def test_cli_rejects_incomplete_conflicting_or_nondecimal_ci_inputs(self):
        invalid = [
            ["--ci-run-id", "123"], ["--ci-run-attempt", "1"],
            ["--matching-tag", "v1.2.3"],
            ["--matching-tag", "--ci-run-id", "123", "--ci-run-attempt", "1"],
            ["v1.2.3", "--ci-run-id", "123", "--ci-run-attempt", "1"],
        ]
        for bad in ["0", "-1", "+1", "01", "1.0", "1e3", " 1", "1 ", "１２", ""]:
            invalid.append(["--ci-run-id", bad, "--ci-run-attempt", "1"])
            invalid.append(["--ci-run-id", "123", "--ci-run-attempt", bad])
        for args in invalid:
            with self.subTest(args=args):
                with mock.patch("sys.stderr", new_callable=io.StringIO):
                    with self.assertRaises(SystemExit) as error:
                        release.main(args, self.root)
                self.assertEqual(error.exception.code, 2)


class CIReleaseGateTests(unittest.TestCase):
    repository = "OpenSpeleo/insta360-rs"
    head = "a" * 40

    def run_payload(self, **changes):
        return {
            "id": 123, "run_attempt": 2,
            "repository": {"full_name": self.repository},
            "head_repository": {"full_name": self.repository},
            "path": ".github/workflows/ci.yml", "event": "push", "head_branch": "master",
            "head_sha": self.head, "status": "completed", "conclusion": "success",
            **changes,
        }

    def check(self, payload):
        return release.ci_run_succeeded(payload, self.repository, 123, 2, self.head)

    def test_successful_master_push_and_manual_branch_or_tag_ci_are_accepted(self):
        self.assertTrue(self.check(self.run_payload()))
        for branch in ["master", "feature", "v1.2.3"]:
            with self.subTest(branch=branch):
                self.assertTrue(self.check(self.run_payload(
                    event="workflow_dispatch", head_branch=branch,
                )))

    def test_ci_identity_commit_event_and_attempt_are_checked(self):
        changes = [
            {"id": 124}, {"id": "123"}, {"id": True},
            {"run_attempt": 1}, {"run_attempt": 3}, {"run_attempt": "2"},
            {"repository": {"full_name": "attacker/insta360-rs"}},
            {"head_repository": {"full_name": "attacker/insta360-rs"}},
            {"repository": None}, {"head_repository": {}},
            {"path": ".github/workflows/release.yml"},
            {"event": "pull_request"}, {"event": "workflow_call"},
            {"event": "workflow_run"}, {"event": "push", "head_branch": "feature"},
            {"head_sha": "b" * 40},
        ]
        for change in changes:
            with self.subTest(change=change):
                with self.assertRaises(ValueError):
                    self.check(self.run_payload(**change))

    def test_terminal_failure_or_incomplete_success_never_authorizes_release(self):
        for conclusion in [None, "failure", "cancelled", "timed_out", "skipped", "neutral", "action_required"]:
            with self.subTest(conclusion=conclusion):
                with self.assertRaisesRegex(ValueError, "did not succeed"):
                    self.check(self.run_payload(conclusion=conclusion))
        with self.assertRaisesRegex(ValueError, "unexpected conclusion"):
            self.check(self.run_payload(status="in_progress"))

    def test_missing_or_invalid_api_fields_fail_closed(self):
        for payload in [None, [], {}, self.run_payload(status="unknown")]:
            with self.subTest(payload=payload):
                with self.assertRaises(ValueError):
                    self.check(payload)
        for field in self.run_payload():
            with self.subTest(missing_field=field):
                payload = self.run_payload()
                del payload[field]
                with self.assertRaises(ValueError):
                    self.check(payload)

    def test_poll_waits_for_dispatch_job_completion(self):
        pending = self.run_payload(status="in_progress", conclusion=None)
        with mock.patch.object(release, "read_ci_run", side_effect=[pending, self.run_payload()]) as read:
            with mock.patch.object(release.time, "monotonic", side_effect=[0, 0, 0, 5]):
                with mock.patch.object(release.time, "sleep") as sleep:
                    with mock.patch("sys.stderr", new_callable=io.StringIO):
                        release.wait_for_ci(self.repository, 123, 2, self.head)
        self.assertEqual(read.call_count, 2)
        read.assert_called_with(self.repository, 123, 30)
        sleep.assert_called_once_with(5)

    def test_every_pending_status_waits(self):
        for status in ["queued", "in_progress", "waiting", "pending", "requested"]:
            with self.subTest(status=status):
                self.assertFalse(self.check(self.run_payload(status=status, conclusion=None)))

    def test_poll_is_bounded(self):
        pending = self.run_payload(status="in_progress", conclusion=None)
        with mock.patch.object(release, "read_ci_run", return_value=pending) as read:
            with mock.patch.object(release.time, "monotonic", side_effect=[0, 0, 0, 5, 5, 10]):
                with mock.patch.object(release.time, "sleep") as sleep:
                    with mock.patch("sys.stderr", new_callable=io.StringIO):
                        with self.assertRaisesRegex(ValueError, "Timed out"):
                            release.wait_for_ci(self.repository, 123, 2, self.head, timeout=10)
        self.assertEqual(read.call_count, 2)
        self.assertEqual(sleep.call_args_list, [mock.call(5), mock.call(5)])
        self.assertEqual(read.call_args_list, [
            mock.call(self.repository, 123, 10), mock.call(self.repository, 123, 5),
        ])

    def test_poll_rechecks_attempt_and_rejects_cancellation_without_more_waiting(self):
        pending = self.run_payload(status="in_progress", conclusion=None)
        for terminal in [self.run_payload(run_attempt=3), self.run_payload(conclusion="cancelled")]:
            with self.subTest(terminal=terminal):
                with mock.patch.object(release, "read_ci_run", side_effect=[pending, terminal]) as read:
                    with mock.patch.object(release.time, "sleep") as sleep:
                        with mock.patch("sys.stderr", new_callable=io.StringIO):
                            with self.assertRaises(ValueError):
                                release.wait_for_ci(self.repository, 123, 2, self.head)
                self.assertEqual(read.call_count, 2)
                sleep.assert_called_once()

    def test_api_error_fails_closed(self):
        for error in [
            subprocess.CalledProcessError(1, ["gh", "api"]),
            subprocess.TimeoutExpired(["gh", "api"], 30),
            json.JSONDecodeError("bad response", "", 0),
        ]:
            with self.subTest(error=error):
                with mock.patch.object(release, "read_ci_run", side_effect=error):
                    with mock.patch.object(release.time, "sleep") as sleep:
                        with self.assertRaises(type(error)):
                            release.wait_for_ci(self.repository, 123, 2, self.head)
                        sleep.assert_not_called()

    def test_api_uses_exact_source_run_endpoint_with_request_timeout(self):
        result = subprocess.CompletedProcess([], 0, stdout=json.dumps(self.run_payload()))
        with mock.patch.object(release.subprocess, "run", return_value=result) as run:
            self.assertEqual(release.read_ci_run(self.repository, 123, 12), self.run_payload())
        run.assert_called_once_with(
            ["gh", "api", "--method", "GET", "repos/OpenSpeleo/insta360-rs/actions/runs/123"],
            check=True, capture_output=True, text=True, timeout=12,
        )


if __name__ == "__main__":
    unittest.main()
