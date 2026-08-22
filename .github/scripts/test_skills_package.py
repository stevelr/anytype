#!/usr/bin/env python3
"""Offline tests for Anytype Toolbox Skills package validation."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import stat
import sys
import tempfile
import unittest
import zipfile


SCRIPT = pathlib.Path(__file__).with_name("validate_skills_package.py")
SPEC = importlib.util.spec_from_file_location("validate_skills_package", SCRIPT)
validator = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = validator
SPEC.loader.exec_module(validator)


def write(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def manifest(version: str, codex: bool) -> dict:
    value = {
        "name": "anytype-toolbox-skills",
        "version": version,
        "description": "Agent Skills for Anytype automation.",
        "author": {"name": "Example Publisher"},
        "homepage": "https://example.com/skills",
        "repository": "https://example.com/source",
        "license": "Apache-2.0",
        "keywords": ["anytype", "automation"],
    }
    if codex:
        value["skills"] = "./skills/"
        value["interface"] = {
            "displayName": "Anytype Toolbox Skills",
            "shortDescription": "Automate Anytype",
            "longDescription": "Use reusable Anytype workflows.",
            "developerName": "Example Publisher",
            "category": "Productivity",
            "capabilities": [],
            "websiteURL": "https://example.com/skills",
            "defaultPrompt": ["Use Anytype Toolbox Skills."],
        }
    return value


def make_package(root: pathlib.Path, version: str = "1.2.3") -> pathlib.Path:
    write(root / ".codex-plugin/plugin.json", json.dumps(manifest(version, True)))
    write(root / ".claude-plugin/plugin.json", json.dumps(manifest(version, False)))
    write(
        root / "CHANGELOG.md",
        f"# Changelog\n\n## [{version}]\n\n### Added\n\n- Initial package.\n",
    )
    write(root / "LICENSE", "Apache License 2.0\n")
    write(root / "README.md", "# Fixture\n\nSee [the skill](skills/example-skill/SKILL.md).\n")
    write(
        root / "skills/example-skill/SKILL.md",
        "---\nname: example-skill\ndescription: Use when validating a fixture.\n"
        "metadata:\n  publisher: fixture\n---\n\n"
        "Read [the reference](references/guide.md).\n",
    )
    write(root / "skills/example-skill/references/guide.md", "# Guide\n")
    return root


def make_zip(source: pathlib.Path, archive: pathlib.Path, prefix: str = "") -> None:
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
        for path in sorted(source.rglob("*")):
            if path.is_file():
                relative = path.relative_to(source).as_posix()
                output.write(path, f"{prefix}{relative}")


class PackageValidationTests(unittest.TestCase):
    def test_valid_release_and_prerelease_directories_pass(self):
        for version in ("1.2.3", "2.0.0-rc.1+build.7"):
            with self.subTest(version=version), tempfile.TemporaryDirectory() as directory:
                identity = validator.validate_package(
                    make_package(pathlib.Path(directory), version)
                )
                self.assertEqual(identity.version, version)
                self.assertEqual(identity.skills, ("example-skill",))

    def test_malformed_frontmatter_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = make_package(pathlib.Path(directory))
            write(
                root / "skills/example-skill/SKILL.md",
                "---\nname example-skill\n---\n\nBody\n",
            )
            with self.assertRaisesRegex(
                validator.PackageValidationError, "expected a key: value"
            ):
                validator.validate_package(root)

    def test_skill_name_must_match_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = make_package(pathlib.Path(directory))
            skill = root / "skills/example-skill/SKILL.md"
            write(
                skill, skill.read_text().replace("name: example-skill", "name: another-skill")
            )
            with self.assertRaisesRegex(
                validator.PackageValidationError, "does not match directory"
            ):
                validator.validate_package(root)

    def test_missing_relative_reference_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = make_package(pathlib.Path(directory))
            (root / "skills/example-skill/references/guide.md").unlink()
            with self.assertRaisesRegex(
                validator.PackageValidationError, "link target does not exist"
            ):
                validator.validate_package(root)

    def test_manifest_and_changelog_version_drift_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = make_package(pathlib.Path(directory))
            write(root / "CHANGELOG.md", "# Changelog\n\n## [1.2.4]\n")
            with self.assertRaisesRegex(
                validator.PackageValidationError, "does not match plugin manifests"
            ):
                validator.validate_package(root)

    def test_host_manifest_version_drift_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = make_package(pathlib.Path(directory))
            write(root / ".claude-plugin/plugin.json", json.dumps(manifest("1.2.4", False)))
            with self.assertRaisesRegex(
                validator.PackageValidationError, "manifests disagree on version"
            ):
                validator.validate_package(root)

    def test_missing_license_and_changelog_fail(self):
        for missing in ("LICENSE", "CHANGELOG.md"):
            with self.subTest(missing=missing), tempfile.TemporaryDirectory() as directory:
                root = make_package(pathlib.Path(directory))
                (root / missing).unlink()
                with self.assertRaisesRegex(
                    validator.PackageValidationError, "required package file is missing"
                ):
                    validator.validate_package(root)

    def test_private_path_and_secret_material_fail(self):
        cases = (
            ("Use /home/alice/project/anytype/scripts/setup.sh.\n", "checkout-private"),
            ("Run source .test-env-nonet before checks.\n", "checkout-private"),
            ("token = ghp_abcdefghijklmnopqrstuvwxyz123456\n", "credential-like"),
        )
        for content, diagnostic in cases:
            with (
                self.subTest(diagnostic=diagnostic),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = make_package(pathlib.Path(directory))
                write(root / "skills/example-skill/references/guide.md", content)
                with self.assertRaisesRegex(validator.PackageValidationError, diagnostic):
                    validator.validate_package(root)

    def test_credential_named_file_fails(self):
        for filename in (".env", "client.key"):
            with self.subTest(filename=filename), tempfile.TemporaryDirectory() as directory:
                root = make_package(pathlib.Path(directory))
                write(root / filename, "ANYTYPE_TOKEN=$TOKEN\n")
                with self.assertRaisesRegex(
                    validator.PackageValidationError, "credential files"
                ):
                    validator.validate_package(root)

    def test_valid_prefixed_archive_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            temporary = pathlib.Path(directory)
            root = make_package(temporary / "package", "3.0.0-beta.2")
            archive = temporary / "skills.zip"
            make_zip(root, archive, "anytype-toolbox-skills/")
            identity = validator.validate_archive(archive, "3.0.0-beta.2")
            self.assertEqual(identity.version, "3.0.0-beta.2")

    def test_archive_parent_traversal_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = pathlib.Path(directory) / "unsafe.zip"
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("../escape", "bad")
            with self.assertRaisesRegex(
                validator.PackageValidationError, "escapes or aliases"
            ):
                validator.validate_archive(archive)

    def test_archive_alias_and_case_collision_fail(self):
        cases = (
            ("root//file", "root/file"),
            ("root/File", "root/file"),
        )
        for first, second in cases:
            with self.subTest(first=first), tempfile.TemporaryDirectory() as directory:
                archive = pathlib.Path(directory) / "unsafe.zip"
                with zipfile.ZipFile(archive, "w") as output:
                    output.writestr(first, "one")
                    output.writestr(second, "two")
                with self.assertRaises(validator.PackageValidationError):
                    validator.validate_archive(archive)

    def test_archive_symlink_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = pathlib.Path(directory) / "symlink.zip"
            info = zipfile.ZipInfo("anytype-toolbox-skills/link")
            info.create_system = 3
            info.external_attr = (stat.S_IFLNK | 0o777) << 16
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr(info, "target")
            with self.assertRaisesRegex(
                validator.PackageValidationError, "not a regular file"
            ):
                validator.validate_archive(archive)

    def test_expected_version_mismatch_fails(self):
        with tempfile.TemporaryDirectory() as directory:
            root = make_package(pathlib.Path(directory), "1.2.3")
            with self.assertRaisesRegex(
                validator.PackageValidationError, "does not match expected version"
            ):
                validator.validate_package(root, "1.2.4-rc.1")


if __name__ == "__main__":
    unittest.main()
