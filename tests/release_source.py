#!/usr/bin/env python3
"""Exercise release tag checks against isolated local Git repositories."""

from pathlib import Path
import os
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / ".github/scripts/verify_release_source.py"


class ReleaseSourceTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(prefix="semctl-release-test-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        self.env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        for key in list(self.env):
            if key.startswith("GIT_CONFIG_KEY_") or key.startswith("GIT_CONFIG_VALUE_"):
                del self.env[key]
        for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_CONFIG_COUNT"]:
            self.env.pop(key, None)
        self.git("init", "--bare", str(self.remote), cwd=self.root)
        self.git("init", str(self.checkout), cwd=self.root)
        self.git("config", "user.name", "Release Test")
        self.git("config", "user.email", "release@example.test")
        self.git("remote", "add", "origin", str(self.remote))
        self.git("commit", "--allow-empty", "-m", "release source")
        self.first = self.git("rev-parse", "HEAD")

    def git(self, *arguments, cwd=None):
        return subprocess.run(
            ["git", *arguments], cwd=cwd or self.checkout, env=self.env,
            check=True, capture_output=True, text=True,
        ).stdout.strip()

    def verify(self, *arguments, commit=None):
        return subprocess.run(
            ["python3", str(SCRIPT), "--tag", "v1.2.3", "--commit", commit or self.first,
             *arguments], cwd=self.checkout, env=self.env, capture_output=True, text=True,
        )

    def publish_tag(self, annotated=False):
        arguments = ["tag", "v1.2.3"]
        if annotated:
            arguments.extend(["-a", "-m", "release"])
        self.git(*arguments)
        self.git("push", "origin", "refs/tags/v1.2.3")

    def test_lightweight_and_annotated_tags_accept_only_the_tagged_commit(self):
        for annotated in [False, True]:
            with self.subTest(annotated=annotated):
                self.publish_tag(annotated)
                accepted = self.verify("--require-tag")
                self.assertEqual(accepted.returncode, 0, accepted.stderr)
                self.git("commit", "--allow-empty", "-m", "later same-version source")
                rejected = self.verify("--require-tag", commit=self.git("rev-parse", "HEAD"))
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn("Run the rebuild from v1.2.3", rejected.stderr)
                self.git("push", "origin", ":refs/tags/v1.2.3")
                self.git("tag", "--delete", "v1.2.3")
                self.git("reset", "--hard", self.first)

    def test_rebuild_requires_a_tag_and_publish_creates_the_exact_source_tag(self):
        self.assertNotEqual(self.verify("--require-tag").returncode, 0)
        self.assertEqual(self.verify().returncode, 0)
        result = self.verify("--create-tag")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("ls-remote", "origin", "refs/tags/v1.2.3").split()[0], self.first)
        self.assertEqual(self.verify("--require-tag").returncode, 0)

    def test_remote_failure_is_not_treated_as_an_absent_tag(self):
        self.git("remote", "set-url", "origin", str(self.root / "missing.git"))
        result = self.verify("--create-tag")
        self.assertNotEqual(result.returncode, 0)

    def test_checked_out_source_must_match_the_requested_commit(self):
        self.git("commit", "--allow-empty", "-m", "different source")
        result = self.verify("--create-tag")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checked-out commit", result.stderr)
        self.assertEqual(self.git("ls-remote", "origin", "refs/tags/v1.2.3"), "")


if __name__ == "__main__":
    unittest.main()
