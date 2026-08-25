#!/usr/bin/env python3
"""Unit tests for release-candidate provenance manifests."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


def load_candidate_module():
    path = Path(__file__).with_name("release_candidate.py")
    spec = importlib.util.spec_from_file_location("release_candidate", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


candidate = load_candidate_module()


class ReleaseCandidateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.lock = self.root / "flake.lock"
        self.lock.write_text("locked\n", encoding="utf-8")
        self.target = "x86_64-unknown-linux-gnu"
        for name in candidate.expected_files("local-artifacts", self.target):
            (self.root / name).write_text(f"{name}\n", encoding="utf-8")

    def create(self):
        return candidate.create_manifest(
            root=self.root,
            kind="local-artifacts",
            repository="owner/repository",
            run_id=123,
            commit="a" * 40,
            target=self.target,
            flake_lock=self.lock,
        )

    def verify(self, manifest):
        candidate.verify_manifest(
            manifest,
            root=self.root,
            kind="local-artifacts",
            repository="owner/repository",
            run_id=123,
            commit="a" * 40,
            target=self.target,
            flake_lock=self.lock,
        )

    def test_exact_candidate_round_trip(self):
        manifest = self.create()
        self.verify(manifest)
        self.assertEqual(manifest["profile"], "nix-static-musl")
        self.assertEqual(
            tuple(manifest["files"]), candidate.expected_files("local-artifacts", self.target)
        )

    def test_wrong_commit_is_rejected(self):
        manifest = self.create()
        manifest["source_commit"] = "b" * 40
        with self.assertRaisesRegex(ValueError, "source_commit"):
            self.verify(manifest)

    def test_wrong_target_is_rejected(self):
        manifest = self.create()
        manifest["target"] = "aarch64-unknown-linux-gnu"
        with self.assertRaisesRegex(ValueError, "target"):
            self.verify(manifest)

    def test_missing_manifest_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "missing"):
            candidate.parse_manifest(self.root / "missing.json")

    def test_wrong_target_file_set_is_rejected(self):
        manifest = self.create()
        manifest["files"]["unexpected"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "file set"):
            self.verify(manifest)

    def test_file_order_is_not_a_trust_boundary(self):
        manifest = self.create()
        manifest["files"] = dict(reversed(tuple(manifest["files"].items())))
        self.verify(manifest)

    def test_changed_candidate_bytes_are_rejected(self):
        manifest = self.create()
        archive = self.root / candidate.expected_files("local-artifacts", self.target)[0]
        archive.write_text("changed\n", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "SHA-256"):
            self.verify(manifest)

    def test_linked_candidate_file_is_rejected(self):
        archive = self.root / candidate.expected_files("local-artifacts", self.target)[0]
        archive.unlink()
        archive.symlink_to(self.lock)
        with self.assertRaisesRegex(ValueError, "linked"):
            self.create()

    def test_macos_input_is_closed_to_the_unsigned_binary(self):
        mac_root = self.root / "mac"
        mac_root.mkdir()
        (mac_root / "anyr").write_bytes(b"mach-o")
        manifest = candidate.create_manifest(
            root=mac_root,
            kind="macos-signing-input",
            repository="owner/repository",
            run_id=123,
            commit="a" * 40,
            target="aarch64-apple-darwin",
            flake_lock=self.lock,
        )
        self.assertEqual(tuple(manifest["files"]), ("anyr",))


if __name__ == "__main__":
    unittest.main()
