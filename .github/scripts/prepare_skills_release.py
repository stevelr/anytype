#!/usr/bin/env python3
"""Validate and reproducibly package an Anytype Toolbox Skills release."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import pathlib
import re
import stat
import sys
import tarfile
import zipfile
from dataclasses import dataclass

import validate_skills_package


TAG_PREFIX = "anytype-toolbox-skills-v"
ARCHIVE_ROOT = validate_skills_package.PLUGIN_NAME
FIXED_ZIP_TIME = (1980, 1, 1, 0, 0, 0)
CHANGELOG_SECTION_PATTERN = re.compile(r"^## (?:\[[^]]+]|[^\n]+)\s*$", re.MULTILINE)


class ReleasePreparationError(ValueError):
    """Reports one deterministic release preparation failure."""


@dataclass(frozen=True)
class ReleaseOutputs:
    """Names of the generated release files."""

    tag: str
    version: str
    zip_path: pathlib.Path
    tar_path: pathlib.Path
    checksum_path: pathlib.Path
    notes_path: pathlib.Path


def require(condition: bool, message: str) -> None:
    """Require a release invariant or report a stable diagnostic."""

    if not condition:
        raise ReleasePreparationError(message)


def version_from_tag(tag: str) -> str:
    """Return the strict semantic version carried by a skills release tag."""

    require(tag.startswith(TAG_PREFIX), f"release tag must start with {TAG_PREFIX!r}")
    version = tag.removeprefix(TAG_PREFIX)
    try:
        return validate_skills_package.validate_semver(version, "release tag version")
    except validate_skills_package.PackageValidationError as error:
        raise ReleasePreparationError(str(error)) from error


def changelog_section(changelog: pathlib.Path, version: str) -> str:
    """Extract exactly one version section, including its heading."""

    try:
        text = changelog.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise ReleasePreparationError(f"CHANGELOG.md: expected UTF-8 text ({error})") from error
    headings = list(CHANGELOG_SECTION_PATTERN.finditer(text))
    matching = [match for match in headings if match.group(0).strip() == f"## [{version}]"]
    require(len(matching) == 1, f"CHANGELOG.md must contain exactly one ## [{version}] section")
    match = matching[0]
    following = next((heading for heading in headings if heading.start() > match.start()), None)
    end = following.start() if following is not None else len(text)
    section = text[match.start() : end].strip()
    require(section != f"## [{version}]", f"CHANGELOG.md section [{version}] must not be empty")
    return section + "\n"


def package_entries(root: pathlib.Path) -> list[pathlib.Path]:
    """Return sorted regular package entries without following symlinks."""

    entries = sorted(root.rglob("*"), key=lambda path: path.relative_to(root).as_posix())
    for entry in entries:
        relative = entry.relative_to(root).as_posix()
        require(not entry.is_symlink(), f"{relative}: symlinks are not permitted")
        require(entry.is_dir() or entry.is_file(), f"{relative}: only regular files and directories are permitted")
    return entries


def archive_name(path: pathlib.Path, root: pathlib.Path, directory: bool = False) -> str:
    """Return a stable POSIX archive path below the plugin root."""

    relative = path.relative_to(root).as_posix()
    name = f"{ARCHIVE_ROOT}/{relative}"
    return f"{name}/" if directory else name


def normalized_mode(path: pathlib.Path) -> int:
    """Normalize archive permissions while preserving executable files."""

    if path.is_dir():
        return 0o755
    return 0o755 if path.stat().st_mode & 0o111 else 0o644


def write_zip(path: pathlib.Path, root: pathlib.Path, entries: list[pathlib.Path]) -> None:
    """Write a deterministic ZIP containing the plugin tree."""

    with zipfile.ZipFile(path, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        root_info = zipfile.ZipInfo(f"{ARCHIVE_ROOT}/", FIXED_ZIP_TIME)
        root_info.create_system = 3
        root_info.external_attr = (stat.S_IFDIR | 0o755) << 16
        root_info.compress_type = zipfile.ZIP_STORED
        archive.writestr(root_info, b"")
        for entry in entries:
            is_directory = entry.is_dir()
            info = zipfile.ZipInfo(archive_name(entry, root, is_directory), FIXED_ZIP_TIME)
            info.create_system = 3
            info.external_attr = (
                (stat.S_IFDIR if is_directory else stat.S_IFREG) | normalized_mode(entry)
            ) << 16
            info.compress_type = zipfile.ZIP_STORED if is_directory else zipfile.ZIP_DEFLATED
            archive.writestr(info, b"" if is_directory else entry.read_bytes())


def tar_info(name: str, mode: int, is_directory: bool, size: int = 0) -> tarfile.TarInfo:
    """Build normalized tar metadata for one entry."""

    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE if is_directory else tarfile.REGTYPE
    info.mode = mode
    info.size = size
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    return info


def write_tar_gz(path: pathlib.Path, root: pathlib.Path, entries: list[pathlib.Path]) -> None:
    """Write a deterministic gzip-compressed USTAR archive."""

    with path.open("xb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                archive.addfile(tar_info(f"{ARCHIVE_ROOT}/", 0o755, True))
                for entry in entries:
                    is_directory = entry.is_dir()
                    name = archive_name(entry, root, is_directory)
                    if is_directory:
                        archive.addfile(tar_info(name, normalized_mode(entry), True))
                        continue
                    data = entry.read_bytes()
                    archive.addfile(
                        tar_info(name, normalized_mode(entry), False, len(data)),
                        io.BytesIO(data),
                    )


def sha256(path: pathlib.Path) -> str:
    """Return the lowercase SHA-256 digest of one file."""

    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def prepare_release(tag: str, package: pathlib.Path, output: pathlib.Path) -> ReleaseOutputs:
    """Validate a tag and plugin tree, then create all release files."""

    version = version_from_tag(tag)
    try:
        validate_skills_package.validate_package(package, expected_version=version)
    except validate_skills_package.PackageValidationError as error:
        raise ReleasePreparationError(str(error)) from error

    package = package.resolve()
    notes = changelog_section(package / "CHANGELOG.md", version)
    output.mkdir(parents=True, exist_ok=True)
    require(not output.is_symlink(), f"{output}: output path must not be a symlink")
    require(output.is_dir(), f"{output}: output path must be a directory")
    require(not any(output.iterdir()), f"{output}: output directory must be empty")

    stem = tag
    zip_path = output / f"{stem}.zip"
    tar_path = output / f"{stem}.tar.gz"
    checksum_path = output / f"{stem}.sha256"
    notes_path = output / f"{stem}-release-notes.md"
    entries = package_entries(package)
    write_zip(zip_path, package, entries)
    write_tar_gz(tar_path, package, entries)
    checksum_path.write_text(
        "".join(f"{sha256(path)}  {path.name}\n" for path in sorted((tar_path, zip_path))),
        encoding="utf-8",
    )
    notes_path.write_text(notes, encoding="utf-8")

    try:
        validate_skills_package.validate_archive(zip_path, expected_version=version)
    except validate_skills_package.PackageValidationError as error:
        raise ReleasePreparationError(f"generated ZIP failed validation: {error}") from error
    return ReleaseOutputs(tag, version, zip_path, tar_path, checksum_path, notes_path)


def parse_arguments() -> argparse.Namespace:
    """Parse command-line arguments."""

    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    tag_parser = subparsers.add_parser("version", help="validate a tag and print its version")
    tag_parser.add_argument("tag")
    prepare_parser = subparsers.add_parser("prepare", help="create release files")
    prepare_parser.add_argument("tag")
    prepare_parser.add_argument("--package", default="skills", type=pathlib.Path)
    prepare_parser.add_argument("--output", required=True, type=pathlib.Path)
    prepare_parser.add_argument("--json", action="store_true")
    return parser.parse_args()


def main() -> int:
    """Validate a tag or prepare a release and return a process status."""

    arguments = parse_arguments()
    try:
        if arguments.command == "version":
            print(version_from_tag(arguments.tag))
            return 0
        outputs = prepare_release(arguments.tag, arguments.package, arguments.output)
    except (OSError, ReleasePreparationError) as error:
        print(f"skills release preparation failed: {error}", file=sys.stderr)
        return 1

    if arguments.json:
        print(
            json.dumps(
                {
                    "tag": outputs.tag,
                    "version": outputs.version,
                    "zip": str(outputs.zip_path),
                    "tar_gz": str(outputs.tar_path),
                    "checksums": str(outputs.checksum_path),
                    "notes": str(outputs.notes_path),
                }
            )
        )
    else:
        print(f"prepared {outputs.tag} in {arguments.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
