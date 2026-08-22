#!/usr/bin/env python3
"""Validate the Anytype Toolbox Skills directory or release ZIP offline."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import shutil
import stat
import sys
import tempfile
import unicodedata
import urllib.parse
import zipfile
from dataclasses import dataclass
from typing import Any, Iterable


PLUGIN_NAME = "anytype-toolbox-skills"
REQUIRED_FILES = (
    ".claude-plugin/plugin.json",
    ".codex-plugin/plugin.json",
    "CHANGELOG.md",
    "LICENSE",
    "README.md",
)
SHARED_MANIFEST_FIELDS = (
    "name",
    "version",
    "description",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
)
SKILL_FIELDS = {
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed-tools",
}
SEMVER_PATTERN = re.compile(
    r"(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)\."
    r"(?:0|[1-9][0-9]*)"
    r"(?:-(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
    r"(?:\.(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*))*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)
SKILL_NAME_PATTERN = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")
MARKDOWN_LINK_PATTERN = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
CHANGELOG_HEADING_PATTERN = re.compile(r"^## \[([^]]+)]\s*$", re.MULTILINE)
PRIVATE_PATH_PATTERNS = (
    re.compile(r"/home/[A-Za-z0-9._-]+/"),
    re.compile(r"/Users/[A-Za-z0-9._-]+/"),
    re.compile(r"[A-Za-z]:\\Users\\[A-Za-z0-9._-]+\\"),
    re.compile(r"~/project/"),
    re.compile(r"(?:^|[\s/])\.test-env(?:-nonet)?(?=$|[\s/])"),
    re.compile(r"\bANYTYPE_CLI_BIN\b"),
    re.compile(r"\bprivate-docs\b"),
)
SECRET_PATTERNS = (
    re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----"),
    re.compile(r"\b(?:sk-[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,})\b"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b"),
    re.compile(
        r"(?i)\b(?:api[_-]?key|password|secret|token)\s*[:=]\s*"
        r"[\"']?[A-Za-z0-9+/=_-]{16,}"
    ),
)
TEXT_SUFFIXES = {
    ".json",
    ".md",
    ".pem",
    ".py",
    ".sh",
    ".toml",
    ".txt",
    ".yaml",
    ".yml",
}
CREDENTIAL_FILENAMES = {"credentials", "credentials.json", "secrets", "secrets.json"}
CREDENTIAL_SUFFIXES = {".jks", ".key", ".keystore", ".p12", ".pfx"}
MAX_ARCHIVE_MEMBERS = 2_048
MAX_ARCHIVE_FILE_BYTES = 16 * 1024 * 1024
MAX_ARCHIVE_TOTAL_BYTES = 128 * 1024 * 1024
MAX_COMPRESSION_RATIO = 200


class PackageValidationError(ValueError):
    """Reports a deterministic package validation failure."""


@dataclass(frozen=True)
class PackageIdentity:
    """Describes the identity established by a valid package."""

    name: str
    version: str
    skills: tuple[str, ...]


def fail(message: str) -> None:
    """Raise one package validation failure with a stable diagnostic."""

    raise PackageValidationError(message)


def require(condition: bool, message: str) -> None:
    """Require a condition or report its stable diagnostic."""

    if not condition:
        fail(message)


def read_utf8(path: pathlib.Path, root: pathlib.Path) -> str:
    """Read a package file as UTF-8 without following symlinks."""

    relative = path.relative_to(root).as_posix()
    require(not path.is_symlink(), f"{relative}: symlinks are not permitted")
    require(path.is_file(), f"{relative}: expected a regular file")
    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        fail(f"{relative}: expected UTF-8 text ({error})")


def load_json(path: pathlib.Path, root: pathlib.Path) -> dict[str, Any]:
    """Load one JSON object from the package."""

    relative = path.relative_to(root).as_posix()
    try:
        value = json.loads(read_utf8(path, root))
    except json.JSONDecodeError as error:
        fail(f"{relative}: malformed JSON at line {error.lineno}, column {error.colno}")
    require(isinstance(value, dict), f"{relative}: top-level JSON value must be an object")
    return value


def parse_scalar(value: str, context: str) -> str:
    """Parse the scalar subset used by Agent Skills frontmatter."""

    value = value.strip()
    require(value != "", f"{context}: value must not be empty")
    if value.startswith('"'):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError as error:
            fail(f"{context}: malformed quoted scalar ({error.msg})")
        require(isinstance(parsed, str), f"{context}: value must be a string")
        return parsed
    if value.startswith("'"):
        require(value.endswith("'") and len(value) >= 2, f"{context}: malformed quoted scalar")
        return value[1:-1].replace("''", "'")

    quoted = False
    result: list[str] = []
    for index, character in enumerate(value):
        if character in {'"', "'"}:
            quoted = not quoted
        if character == "#" and not quoted and index > 0 and value[index - 1].isspace():
            break
        result.append(character)
    parsed = "".join(result).strip()
    require(parsed != "", f"{context}: value must not be empty")
    require(
        parsed.lower() not in {"null", "true", "false", "~"}
        and not parsed.startswith(("[", "{", "&", "*", "!")),
        f"{context}: value must be a string",
    )
    return parsed


def parse_frontmatter(text: str, context: str) -> dict[str, Any]:
    """Parse the Agent Skills frontmatter subset defined by the standard."""

    lines = text.splitlines()
    require(lines and lines[0] == "---", f"{context}: frontmatter must start with ---")
    try:
        end = lines.index("---", 1)
    except ValueError:
        fail(f"{context}: frontmatter is missing its closing ---")
    require(
        any(line.strip() for line in lines[end + 1 :]), f"{context}: instruction body is empty"
    )

    metadata: dict[str, Any] = {}
    index = 1
    while index < end:
        line = lines[index]
        index += 1
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        require(line == line.lstrip(), f"{context}:{index}: unexpected indentation")
        require(":" in line, f"{context}:{index}: expected a key: value entry")
        key, raw_value = line.split(":", 1)
        key = key.strip()
        require(
            key in SKILL_FIELDS, f"{context}:{index}: unsupported frontmatter field {key!r}"
        )
        require(key not in metadata, f"{context}:{index}: duplicate frontmatter field {key!r}")

        raw_value = raw_value.strip()
        if key == "metadata":
            require(raw_value == "", f"{context}:{index}: metadata must be a string map")
            values: dict[str, str] = {}
            metadata_indent: int | None = None
            while index < end and lines[index].startswith((" ", "\t")):
                nested = lines[index]
                index += 1
                require(
                    "\t" not in nested[: len(nested) - len(nested.lstrip())],
                    f"{context}:{index}: metadata indentation must use spaces",
                )
                indent = len(nested) - len(nested.lstrip(" "))
                require(indent > 0, f"{context}:{index}: metadata entry must be indented")
                if metadata_indent is None:
                    metadata_indent = indent
                require(
                    indent == metadata_indent,
                    f"{context}:{index}: metadata entries must use consistent indentation",
                )
                nested = nested.strip()
                require(
                    ":" in nested, f"{context}:{index}: expected a metadata key: value entry"
                )
                nested_key, nested_value = nested.split(":", 1)
                nested_key = nested_key.strip()
                require(nested_key != "", f"{context}:{index}: metadata key must not be empty")
                require(
                    nested_key not in values,
                    f"{context}:{index}: duplicate metadata key {nested_key!r}",
                )
                values[nested_key] = parse_scalar(nested_value, f"{context}:{index}")
            metadata[key] = values
            continue

        if raw_value in {">", "|"}:
            block: list[str] = []
            while index < end and lines[index].startswith((" ", "\t")):
                block.append(lines[index].strip())
                index += 1
            require(block, f"{context}:{index}: block scalar must not be empty")
            metadata[key] = (" " if raw_value == ">" else "\n").join(block)
        else:
            metadata[key] = parse_scalar(raw_value, f"{context}:{index}")
    return metadata


def validate_semver(version: Any, context: str) -> str:
    """Return a valid semantic version string."""

    require(isinstance(version, str), f"{context}: version must be a string")
    require(len(version) <= 64, f"{context}: version must be 64 characters or fewer")
    require(
        SEMVER_PATTERN.fullmatch(version) is not None,
        f"{context}: invalid semantic version {version!r}",
    )
    return version


def safe_package_path(root: pathlib.Path, value: str, context: str) -> pathlib.Path:
    """Resolve one manifest path while keeping it inside the plugin root."""

    require(value.startswith("./"), f"{context}: path must start with ./")
    require(
        "\\" not in value and "\x00" not in value, f"{context}: path uses unsafe separators"
    )
    relative = pathlib.PurePosixPath(value[2:])
    require(
        relative.parts and ".." not in relative.parts,
        f"{context}: path escapes the plugin root",
    )
    candidate = (root / pathlib.Path(*relative.parts)).resolve()
    try:
        candidate.relative_to(root.resolve())
    except ValueError:
        fail(f"{context}: path escapes the plugin root")
    require(candidate.exists(), f"{context}: referenced path does not exist: {value}")
    return candidate


def manifest_paths(manifest: dict[str, Any], host: str) -> Iterable[tuple[str, str]]:
    """Yield path-valued fields supported by a host manifest."""

    fields = (
        ("skills", "hooks", "mcpServers", "apps")
        if host == "codex"
        else (
            "commands",
            "agents",
            "hooks",
            "mcpServers",
        )
    )
    for field in fields:
        value = manifest.get(field)
        if isinstance(value, str):
            yield field, value
        elif isinstance(value, list):
            for index, item in enumerate(value):
                if isinstance(item, str):
                    yield f"{field}[{index}]", item
                else:
                    require(
                        field == "hooks" and isinstance(item, dict),
                        f"{host} manifest {field}[{index}]: expected a relative path",
                    )
        elif value is not None:
            require(
                field in {"hooks", "mcpServers"} and isinstance(value, dict),
                f"{host} manifest {field}: expected a relative path",
            )

    interface = manifest.get("interface")
    if isinstance(interface, dict):
        for field in ("composerIcon", "logo", "logoDark"):
            value = interface.get(field)
            if isinstance(value, str):
                yield f"interface.{field}", value
        screenshots = interface.get("screenshots")
        if isinstance(screenshots, list):
            for index, value in enumerate(screenshots):
                require(
                    isinstance(value, str),
                    f"{host} manifest interface.screenshots[{index}]: path must be a string",
                )
                yield f"interface.screenshots[{index}]", value
        elif screenshots is not None:
            fail(f"{host} manifest interface.screenshots: expected a path list")


def validate_manifest(manifest: dict[str, Any], host: str, root: pathlib.Path) -> None:
    """Validate one host manifest and every declared package path."""

    name = manifest.get("name")
    require(
        isinstance(name, str) and name == PLUGIN_NAME,
        f"{host} manifest: name must be {PLUGIN_NAME!r}",
    )
    validate_semver(manifest.get("version"), f"{host} manifest")
    description = manifest.get("description")
    require(
        isinstance(description, str) and 1 <= len(description) <= 1_024,
        f"{host} manifest: description must contain 1-1024 characters",
    )
    author = manifest.get("author")
    require(isinstance(author, dict), f"{host} manifest: author must be an object")
    author_name = author.get("name") if isinstance(author, dict) else None
    require(
        isinstance(author_name, str) and 1 <= len(author_name) <= 120,
        f"{host} manifest: author.name must contain 1-120 characters",
    )
    license_name = manifest.get("license")
    require(
        isinstance(license_name, str) and license_name != "",
        f"{host} manifest: license must be non-empty",
    )

    if host == "codex":
        require(
            manifest.get("skills") == "./skills/", "codex manifest: skills must be ./skills/"
        )
        interface = manifest.get("interface")
        require(isinstance(interface, dict), "codex manifest: interface must be an object")
        require(
            interface.get("developerName") == author_name,
            "codex manifest: interface.developerName must match author.name",
        )

    for field, value in manifest_paths(manifest, host):
        safe_package_path(root, value, f"{host} manifest {field}")


def validate_skill(skill_root: pathlib.Path, package_root: pathlib.Path) -> str:
    """Validate one immediate child of the plugin skills directory."""

    relative = skill_root.relative_to(package_root).as_posix()
    require(not skill_root.is_symlink(), f"{relative}: skill directory must not be a symlink")
    require(skill_root.is_dir(), f"{relative}: expected a skill directory")
    skill_file = skill_root / "SKILL.md"
    metadata = parse_frontmatter(read_utf8(skill_file, package_root), f"{relative}/SKILL.md")
    name = metadata.get("name")
    require(isinstance(name, str), f"{relative}/SKILL.md: name is required")
    require(
        SKILL_NAME_PATTERN.fullmatch(name) is not None and len(name) <= 64,
        f"{relative}/SKILL.md: invalid skill name {name!r}",
    )
    require(
        name == skill_root.name,
        f"{relative}/SKILL.md: name {name!r} does not match directory {skill_root.name!r}",
    )
    description = metadata.get("description")
    require(
        isinstance(description, str) and 1 <= len(description) <= 1_024,
        f"{relative}/SKILL.md: description must contain 1-1024 characters",
    )
    compatibility = metadata.get("compatibility")
    require(
        compatibility is None or 1 <= len(compatibility) <= 500,
        f"{relative}/SKILL.md: compatibility must contain 1-500 characters",
    )
    return name


def markdown_destination(raw: str) -> str:
    """Return the path portion of one Markdown link destination."""

    raw = raw.strip()
    if raw.startswith("<"):
        closing = raw.find(">")
        require(closing > 1, f"malformed Markdown link destination {raw!r}")
        return raw[1:closing]
    return raw.split(maxsplit=1)[0]


def validate_markdown_links(root: pathlib.Path) -> None:
    """Require every package-relative Markdown link to resolve in the package."""

    for path in sorted(root.rglob("*.md")):
        text = read_utf8(path, root)
        relative = path.relative_to(root).as_posix()
        for match in MARKDOWN_LINK_PATTERN.finditer(text):
            destination = markdown_destination(match.group(1))
            parsed = urllib.parse.urlsplit(destination)
            if parsed.scheme or parsed.netloc or destination.startswith("#"):
                continue
            decoded = urllib.parse.unquote(parsed.path)
            if decoded == "":
                continue
            require(
                "\\" not in decoded, f"{relative}: link uses a backslash path: {destination}"
            )
            target = (path.parent / decoded).resolve()
            try:
                target.relative_to(root.resolve())
            except ValueError:
                fail(f"{relative}: link escapes the plugin root: {destination}")
            require(target.exists(), f"{relative}: link target does not exist: {destination}")


def is_text_file(path: pathlib.Path) -> bool:
    """Return whether a package file belongs to the public text surface."""

    return path.suffix.lower() in TEXT_SUFFIXES or path.name in {
        "LICENSE",
        "README",
        "CHANGELOG",
    }


def validate_public_text(root: pathlib.Path) -> None:
    """Reject private checkout details and credential material in public text."""

    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        require(not path.is_symlink(), f"{relative}: symlinks are not permitted")
        require(
            path.is_file() or path.is_dir(),
            f"{relative}: only regular files and directories are permitted",
        )
        filename = path.name.casefold()
        require(
            filename not in CREDENTIAL_FILENAMES
            and filename != ".env"
            and not filename.startswith(".env."),
            f"{relative}: credential files are not permitted",
        )
        require(
            path.suffix.casefold() not in CREDENTIAL_SUFFIXES,
            f"{relative}: credential files are not permitted",
        )
        if not path.is_file() or not is_text_file(path):
            continue
        text = read_utf8(path, root)
        for pattern in PRIVATE_PATH_PATTERNS:
            require(
                pattern.search(text) is None,
                f"{relative}: contains checkout-private path or test guidance",
            )
        for pattern in SECRET_PATTERNS:
            require(
                pattern.search(text) is None, f"{relative}: contains credential-like material"
            )


def changelog_version(path: pathlib.Path, root: pathlib.Path) -> str:
    """Return the first released semantic version in the package changelog."""

    headings = CHANGELOG_HEADING_PATTERN.findall(read_utf8(path, root))
    versions = [heading for heading in headings if heading.lower() != "unreleased"]
    require(versions, "CHANGELOG.md: no released version heading found")
    return validate_semver(versions[0], "CHANGELOG.md first release heading")


def validate_package(
    root: pathlib.Path, expected_version: str | None = None
) -> PackageIdentity:
    """Validate one unpacked plugin directory."""

    require(root.exists(), f"{root}: package root does not exist")
    require(root.is_dir(), f"{root}: package root must be a directory")
    require(not root.is_symlink(), f"{root}: package root must not be a symlink")
    root = root.resolve()
    for relative in REQUIRED_FILES:
        require((root / relative).is_file(), f"{relative}: required package file is missing")

    codex = load_json(root / ".codex-plugin/plugin.json", root)
    claude = load_json(root / ".claude-plugin/plugin.json", root)
    validate_manifest(codex, "codex", root)
    validate_manifest(claude, "claude", root)
    for field in SHARED_MANIFEST_FIELDS:
        require(codex.get(field) == claude.get(field), f"plugin manifests disagree on {field}")

    version = validate_semver(codex.get("version"), "plugin manifests")
    require(
        changelog_version(root / "CHANGELOG.md", root) == version,
        "CHANGELOG.md first release version does not match plugin manifests",
    )
    if expected_version is not None:
        validate_semver(expected_version, "expected version")
        require(
            version == expected_version,
            f"plugin version {version!r} does not match expected version {expected_version!r}",
        )

    skills_root = root / "skills"
    require(
        skills_root.is_dir() and not skills_root.is_symlink(),
        "skills/: required skill directory is missing",
    )
    skill_directories = sorted(path for path in skills_root.iterdir() if path.is_dir())
    require(skill_directories, "skills/: at least one skill is required")
    skills = tuple(validate_skill(path, root) for path in skill_directories)
    validate_markdown_links(root)
    validate_public_text(root)
    return PackageIdentity(name=PLUGIN_NAME, version=version, skills=skills)


def validate_archive_member(info: zipfile.ZipInfo) -> pathlib.PurePosixPath:
    """Validate one ZIP member name, mode, encryption flag, and size."""

    name = info.filename
    require(name != "" and "\x00" not in name, "archive contains an empty or NUL path")
    require("\\" not in name, f"archive member uses a backslash path: {name!r}")
    require(
        all(ord(character) >= 32 for character in name),
        f"archive member uses control characters: {name!r}",
    )
    path = pathlib.PurePosixPath(name)
    require(
        not path.is_absolute() and path.parts,
        f"archive member uses an absolute path: {name!r}",
    )
    archive_spelling = name[:-1] if name.endswith("/") else name
    require(
        ".." not in path.parts and path.as_posix() == archive_spelling,
        f"archive member escapes or aliases its root: {name!r}",
    )
    require(
        not re.match(r"^[A-Za-z]:", path.parts[0]),
        f"archive member uses a drive path: {name!r}",
    )
    require(info.flag_bits & 0x1 == 0, f"archive member is encrypted: {name!r}")
    require(
        info.file_size <= MAX_ARCHIVE_FILE_BYTES,
        f"archive member exceeds the size limit: {name!r}",
    )
    if info.file_size > 0:
        require(
            info.compress_size > 0, f"archive member has an invalid compressed size: {name!r}"
        )
        require(
            info.file_size <= info.compress_size * MAX_COMPRESSION_RATIO,
            f"archive member exceeds the compression-ratio limit: {name!r}",
        )

    mode = info.external_attr >> 16
    file_type = stat.S_IFMT(mode)
    require(
        file_type in {0, stat.S_IFREG, stat.S_IFDIR},
        f"archive member is not a regular file or directory: {name!r}",
    )
    return path


def archive_plugin_prefix(paths: list[pathlib.PurePosixPath]) -> pathlib.PurePosixPath:
    """Return the sole plugin root prefix represented by safe ZIP paths."""

    codex_manifest = pathlib.PurePosixPath(".codex-plugin/plugin.json")
    if codex_manifest in paths:
        return pathlib.PurePosixPath()
    top_levels = {path.parts[0] for path in paths if path.parts}
    require(
        len(top_levels) == 1, "archive must contain one plugin root and no sibling entries"
    )
    prefix = pathlib.PurePosixPath(next(iter(top_levels)))
    require(
        prefix / codex_manifest in paths,
        "archive plugin root is missing .codex-plugin/plugin.json",
    )
    return prefix


def validate_archive(
    path: pathlib.Path, expected_version: str | None = None
) -> PackageIdentity:
    """Validate a release ZIP without trusting its entry paths or metadata."""

    require(path.is_file(), f"{path}: archive does not exist")
    require(path.suffix.lower() == ".zip", f"{path}: expected a .zip archive")
    try:
        archive = zipfile.ZipFile(path)
    except (OSError, zipfile.BadZipFile) as error:
        fail(f"{path}: unreadable ZIP archive ({error})")

    with archive:
        infos = archive.infolist()
        require(
            0 < len(infos) <= MAX_ARCHIVE_MEMBERS,
            "archive member count is outside the permitted range",
        )
        paths = [validate_archive_member(info) for info in infos]
        folded = [
            unicodedata.normalize("NFC", member.as_posix()).casefold() for member in paths
        ]
        require(
            len(folded) == len(set(folded)),
            "archive contains duplicate or case-colliding member paths",
        )
        total_bytes = sum(info.file_size for info in infos)
        require(
            total_bytes <= MAX_ARCHIVE_TOTAL_BYTES,
            "archive exceeds the total uncompressed size limit",
        )
        prefix = archive_plugin_prefix(paths)

        with tempfile.TemporaryDirectory(prefix="anytype-skills-validate-") as temporary:
            extraction_root = pathlib.Path(temporary)
            for info, member in zip(infos, paths, strict=True):
                target = extraction_root.joinpath(*member.parts)
                if info.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(info) as source, target.open("xb") as destination:
                    shutil.copyfileobj(source, destination)
            plugin_root = extraction_root.joinpath(*prefix.parts)
            return validate_package(plugin_root, expected_version)


def parse_arguments() -> argparse.Namespace:
    """Parse command-line arguments."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package", nargs="?", default="skills", type=pathlib.Path)
    parser.add_argument(
        "--expected-version", help="require this exact release or prerelease version"
    )
    parser.add_argument(
        "--json", action="store_true", help="print the validated identity as JSON"
    )
    return parser.parse_args()


def main() -> int:
    """Run directory or ZIP validation and return a process status."""

    arguments = parse_arguments()
    try:
        if arguments.package.suffix.lower() == ".zip":
            identity = validate_archive(arguments.package, arguments.expected_version)
        else:
            identity = validate_package(arguments.package, arguments.expected_version)
    except PackageValidationError as error:
        print(f"skills package validation failed: {error}", file=sys.stderr)
        return 1
    except (OSError, zipfile.BadZipFile) as error:
        print(f"skills package validation failed: {error}", file=sys.stderr)
        return 1

    if arguments.json:
        print(
            json.dumps(
                {"name": identity.name, "version": identity.version, "skills": identity.skills}
            )
        )
    else:
        print(f"validated {identity.name} {identity.version}: {', '.join(identity.skills)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
