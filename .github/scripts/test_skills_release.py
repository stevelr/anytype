#!/usr/bin/env python3
"""Offline tests for deterministic skills release preparation and routing."""

from __future__ import annotations

import fnmatch
import hashlib
import json
import pathlib
import shutil
import stat
import sys
import tarfile
import tempfile
import unittest
import zipfile


SCRIPT_ROOT = pathlib.Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_ROOT.parents[1]
CURRENT_VERSION = json.loads(
    (REPOSITORY_ROOT / "skills/.codex-plugin/plugin.json").read_text(encoding="utf-8")
)["version"]
sys.path.insert(0, str(SCRIPT_ROOT))

import prepare_skills_release as release  # noqa: E402


def mapping_block(document: str, name: str, indent: int) -> str:
    """Return one indentation-delimited YAML mapping block."""
    marker = f"{' ' * indent}{name}:"
    lines = document.splitlines()
    start = next((index for index, line in enumerate(lines) if line.rstrip() == marker), None)
    if start is None:
        raise AssertionError(f"workflow has no {name!r} mapping at indent {indent}")
    end = len(lines)
    for index, raw_line in enumerate(lines[start + 1 :], start + 1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        line_indent = len(raw_line) - len(raw_line.lstrip(" "))
        if line_indent <= indent:
            end = index
            break
    return "\n".join(lines[start:end])


def mapping_entries(document: str, name: str, indent: int) -> dict[str, str]:
    """Return direct keys and scalar values from one YAML mapping."""
    block = mapping_block(document, name, indent)
    entries: dict[str, str] = {}
    for raw_line in block.splitlines()[1:]:
        line = raw_line.strip()
        line_indent = len(raw_line) - len(raw_line.lstrip(" "))
        if not line or line.startswith("#") or line_indent != indent + 2:
            continue
        key, separator, value = line.partition(":")
        if not separator:
            raise AssertionError(f"invalid {name} mapping entry {line!r}")
        if key in entries:
            raise AssertionError(f"duplicate {name} mapping key {key!r}")
        entries[key] = value.strip()
    return entries


class SkillsReleaseTests(unittest.TestCase):
    """Exercise release identity, notes, archives, and workflow routing."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="skills-release-test-")
        self.root = pathlib.Path(self.temporary.name)
        self.package = self.root / "skills"
        shutil.copytree(REPOSITORY_ROOT / "skills", self.package)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def prepare(self, tag: str | None = None, name: str = "dist") -> release.ReleaseOutputs:
        tag = tag or f"anytype-toolbox-skills-v{CURRENT_VERSION}"
        return release.prepare_release(tag, self.package, self.root / name)

    def set_package_version(self, version: str) -> None:
        """Set both fixture manifests and its release heading to one version."""

        for relative in (".codex-plugin/plugin.json", ".claude-plugin/plugin.json"):
            path = self.package / relative
            manifest = json.loads(path.read_text(encoding="utf-8"))
            manifest["version"] = version
            path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        changelog = self.package / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                f"## [{CURRENT_VERSION}]", f"## [{version}]"
            ),
            encoding="utf-8",
        )

    def test_accepts_stable_and_prerelease_tags(self) -> None:
        self.assertEqual(release.version_from_tag("anytype-toolbox-skills-v1.2.3"), "1.2.3")
        self.assertEqual(
            release.version_from_tag("anytype-toolbox-skills-v2.0.0-rc.1+build.5"),
            "2.0.0-rc.1+build.5",
        )

    def test_rejects_malformed_or_unrelated_tags(self) -> None:
        for tag in (
            "anyr-v1.2.3",
            "anytype-toolbox-skills-v1.2",
            "anytype-toolbox-skills-v01.2.3",
            "anytype-toolbox-skills-v1.2.3-01",
            "anytype-toolbox-skills-v1.2.3_qa",
        ):
            with self.subTest(tag=tag), self.assertRaises(release.ReleasePreparationError):
                release.version_from_tag(tag)

    def test_rejects_tag_manifest_version_mismatch(self) -> None:
        with self.assertRaisesRegex(
            release.ReleasePreparationError, "does not match expected version"
        ):
            self.prepare("anytype-toolbox-skills-v0.2.0")

    def test_prepares_prerelease_package(self) -> None:
        self.set_package_version("0.2.0-rc.1")
        outputs = self.prepare("anytype-toolbox-skills-v0.2.0-rc.1")
        self.assertEqual(outputs.version, "0.2.0-rc.1")
        self.assertTrue(outputs.zip_path.is_file())

    def test_rejects_changelog_version_mismatch(self) -> None:
        changelog = self.package / "CHANGELOG.md"
        changelog.write_text(
            changelog.read_text(encoding="utf-8").replace(
                f"## [{CURRENT_VERSION}]", "## [0.0.9]"
            ),
            encoding="utf-8",
        )
        with self.assertRaisesRegex(
            release.ReleasePreparationError, "first release version does not match"
        ):
            self.prepare()

    def test_release_notes_are_exact_version_section(self) -> None:
        changelog = self.package / "CHANGELOG.md"
        changelog.write_text(
            "# Changelog\n\n## [Unreleased]\n\n- Later.\n\n"
            f"## [{CURRENT_VERSION}]\n\n### Added\n\n- Selected.\n\n"
            "## [0.0.1]\n\n- Earlier.\n",
            encoding="utf-8",
        )
        outputs = self.prepare()
        self.assertEqual(
            outputs.notes_path.read_text(encoding="utf-8"),
            f"## [{CURRENT_VERSION}]\n\n### Added\n\n- Selected.\n",
        )

    def test_release_requires_nonempty_matching_changelog_section(self) -> None:
        changelog = self.package / "CHANGELOG.md"
        changelog.write_text(f"# Changelog\n\n## [{CURRENT_VERSION}]\n", encoding="utf-8")
        with self.assertRaisesRegex(release.ReleasePreparationError, "must not be empty"):
            self.prepare()

    def test_archives_are_reproducible_and_contain_only_plugin_tree(self) -> None:
        first = self.prepare(name="first")
        second = self.prepare(name="second")
        self.assertEqual(first.zip_path.read_bytes(), second.zip_path.read_bytes())
        self.assertEqual(first.tar_path.read_bytes(), second.tar_path.read_bytes())

        expected_files = {
            f"{release.ARCHIVE_ROOT}/{path.relative_to(self.package).as_posix()}"
            for path in self.package.rglob("*")
            if path.is_file()
        }
        with zipfile.ZipFile(first.zip_path) as archive:
            zip_files = {info.filename for info in archive.infolist() if not info.is_dir()}
            self.assertEqual(zip_files, expected_files)
            self.assertTrue(
                all(info.date_time == release.FIXED_ZIP_TIME for info in archive.infolist())
            )
        with tarfile.open(first.tar_path, "r:gz") as archive:
            tar_files = {member.name for member in archive.getmembers() if member.isfile()}
            self.assertEqual(tar_files, expected_files)
            self.assertTrue(
                all(
                    member.mtime == 0 and member.uid == 0 and member.gid == 0
                    for member in archive.getmembers()
                )
            )
            for source in self.package.rglob("*"):
                if not source.is_file():
                    continue
                extracted = archive.extractfile(
                    f"{release.ARCHIVE_ROOT}/{source.relative_to(self.package).as_posix()}"
                )
                self.assertIsNotNone(extracted)
                assert extracted is not None
                self.assertEqual(extracted.read(), source.read_bytes())

    def test_checksums_cover_only_release_archives(self) -> None:
        outputs = self.prepare()
        lines = outputs.checksum_path.read_text(encoding="utf-8").splitlines()
        expected = {
            path.name: hashlib.sha256(path.read_bytes()).hexdigest()
            for path in (outputs.zip_path, outputs.tar_path)
        }
        actual = {line.split("  ", 1)[1]: line.split("  ", 1)[0] for line in lines}
        self.assertEqual(actual, expected)

    def test_archive_modes_are_normalized(self) -> None:
        executable = self.package / "skills/anyr/example.sh"
        executable.write_text("#!/bin/sh\n", encoding="utf-8")
        executable.chmod(0o755)
        outputs = self.prepare()
        member_name = f"{release.ARCHIVE_ROOT}/skills/anyr/example.sh"
        with zipfile.ZipFile(outputs.zip_path) as archive:
            mode = archive.getinfo(member_name).external_attr >> 16
            self.assertEqual(stat.S_IMODE(mode), 0o755)
        with tarfile.open(outputs.tar_path, "r:gz") as archive:
            self.assertEqual(archive.getmember(member_name).mode, 0o755)

    def test_output_directory_must_be_empty(self) -> None:
        output = self.root / "dist"
        output.mkdir()
        (output / "foreign").write_text("keep", encoding="utf-8")
        with self.assertRaisesRegex(release.ReleasePreparationError, "must be empty"):
            release.prepare_release(
                f"anytype-toolbox-skills-v{CURRENT_VERSION}", self.package, output
            )
        self.assertEqual((output / "foreign").read_text(encoding="utf-8"), "keep")


class WorkflowRoutingTests(unittest.TestCase):
    """Prove Rust CLI and skills tags route to disjoint workflows."""

    @staticmethod
    def tag_patterns(path: pathlib.Path) -> list[str]:
        lines = path.read_text(encoding="utf-8").splitlines()
        tags_index = next(index for index, line in enumerate(lines) if line.strip() == "tags:")
        patterns: list[str] = []
        for line in lines[tags_index + 1 :]:
            stripped = line.strip()
            if not stripped.startswith("- "):
                break
            patterns.append(stripped.removeprefix("- ").strip('"'))
        return patterns

    def test_release_workflow_tag_routes_are_disjoint(self) -> None:
        rust_patterns = self.tag_patterns(REPOSITORY_ROOT / ".github/workflows/release.yml")
        skills_patterns = self.tag_patterns(
            REPOSITORY_ROOT / ".github/workflows/skills-release.yml"
        )

        def matches(tag: str, patterns: list[str]) -> bool:
            return any(fnmatch.fnmatchcase(tag, pattern) for pattern in patterns)

        for tag in ("1.2.3", "anyr-v1.2.3"):
            self.assertTrue(matches(tag, rust_patterns))
            self.assertFalse(matches(tag, skills_patterns))
        skills_tag = "anytype-toolbox-skills-v1.2.3"
        self.assertTrue(matches(skills_tag, skills_patterns))
        self.assertFalse(matches(skills_tag, rust_patterns))
        self.assertEqual(skills_patterns, ["anytype-toolbox-skills-v*"])

    def test_workflow_preserves_nonlatest_release_and_minimal_permissions(self) -> None:
        workflow = (REPOSITORY_ROOT / ".github/workflows/skills-release.yml").read_text(
            encoding="utf-8"
        )
        self.assertEqual(mapping_entries(workflow, "on", 0), {"push": ""})
        self.assertEqual(mapping_entries(workflow, "permissions", 0), {"contents": "read"})
        package = mapping_block(workflow, "package", 2)
        self.assertEqual(mapping_entries(package, "permissions", 4), {"contents": "read"})
        publish = mapping_block(workflow, "publish", 2)
        self.assertEqual(mapping_entries(workflow, "publish", 2)["needs"], "package")
        self.assertEqual(mapping_entries(publish, "permissions", 4), {"contents": "write"})
        for line in workflow.splitlines():
            stripped = line.strip()
            if stripped.startswith("- uses:") or stripped.startswith("uses:"):
                reference = stripped.split("@", 1)[1].split()[0]
                self.assertRegex(reference, r"^[0-9a-f]{40}$", stripped)
        self.assertIn('gh release create "$RELEASE_TAG"', workflow)
        self.assertIn("--latest=false", workflow)


if __name__ == "__main__":
    unittest.main()
