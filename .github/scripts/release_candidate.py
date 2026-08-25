#!/usr/bin/env python3
"""Create and verify immutable release-candidate provenance manifests."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import sys


PRODUCER_WORKFLOW = ".github/workflows/release.yml"
PROFILES = {
    "aarch64-apple-darwin": "nix-darwin",
    "aarch64-unknown-linux-gnu": "nix-static-musl",
    "aarch64-pc-windows-msvc": "cargo-dist",
    "x86_64-unknown-linux-gnu": "nix-static-musl",
    "x86_64-pc-windows-msvc": "cargo-dist",
}
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
REPOSITORY = re.compile(r"^[^/\s]+/[^/\s]+$")


def sha256_file(path: Path) -> str:
    """Return the SHA-256 digest of one regular file."""

    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def expected_files(kind: str, target: str) -> tuple[str, ...]:
    """Return the closed file set for one candidate handoff kind."""

    if target not in PROFILES:
        raise ValueError(f"unsupported release target: {target}")
    if kind == "macos-signing-input":
        if target != "aarch64-apple-darwin":
            raise ValueError("macOS signing input requires aarch64-apple-darwin")
        return ("anyr",)
    if kind != "local-artifacts":
        raise ValueError(f"unsupported candidate kind: {kind}")
    extension = "zip" if target.endswith("windows-msvc") else "tar.xz"
    archive = f"anyr-{target}.{extension}"
    return (archive, f"{archive}.sha256", f"{target}-dist-manifest.json")


def checked_file(root: Path, name: str) -> Path:
    """Resolve one candidate file without admitting links or empty files."""

    path = root / name
    if not path.is_file() or path.is_symlink() or path.stat().st_size == 0:
        raise ValueError(f"candidate file is missing, linked, or empty: {name}")
    return path


def validate_identity(repository: str, run_id: int, commit: str, target: str) -> None:
    """Validate provenance identity fields before reading candidate bytes."""

    if not REPOSITORY.fullmatch(repository):
        raise ValueError("repository must be OWNER/NAME")
    if run_id <= 0:
        raise ValueError("source run ID must be positive")
    if not COMMIT.fullmatch(commit):
        raise ValueError("source commit must be a lowercase 40-character SHA")
    if target not in PROFILES:
        raise ValueError(f"unsupported release target: {target}")


def create_manifest(
    *,
    root: Path,
    kind: str,
    repository: str,
    run_id: int,
    commit: str,
    target: str,
    flake_lock: Path,
) -> dict:
    """Create a provenance manifest for a closed candidate file set."""

    validate_identity(repository, run_id, commit, target)
    lock_hash = sha256_file(checked_file(flake_lock.parent, flake_lock.name))
    names = expected_files(kind, target)
    files = {name: sha256_file(checked_file(root, name)) for name in names}
    return {
        "schema_version": 1,
        "kind": kind,
        "repository": repository,
        "producer_workflow": PRODUCER_WORKFLOW,
        "source_run_id": run_id,
        "source_ref": "refs/heads/main",
        "source_commit": commit,
        "target": target,
        "profile": PROFILES[target],
        "flake_lock_sha256": lock_hash,
        "files": files,
    }


def verify_manifest(
    manifest: dict,
    *,
    root: Path,
    kind: str,
    repository: str,
    run_id: int,
    commit: str,
    target: str,
    flake_lock: Path,
) -> None:
    """Verify candidate identity, locked inputs, file set, and file hashes."""

    validate_identity(repository, run_id, commit, target)
    names = expected_files(kind, target)
    expected_identity = {
        "schema_version": 1,
        "kind": kind,
        "repository": repository,
        "producer_workflow": PRODUCER_WORKFLOW,
        "source_run_id": run_id,
        "source_ref": "refs/heads/main",
        "source_commit": commit,
        "target": target,
        "profile": PROFILES[target],
        "flake_lock_sha256": sha256_file(checked_file(flake_lock.parent, flake_lock.name)),
    }
    for field, expected in expected_identity.items():
        if manifest.get(field) != expected:
            raise ValueError(f"candidate manifest has unexpected {field}")
    files = manifest.get("files")
    if not isinstance(files, dict) or set(files) != set(names):
        raise ValueError("candidate manifest has an unexpected file set")
    for name in names:
        recorded = files.get(name)
        if not isinstance(recorded, str) or not SHA256.fullmatch(recorded):
            raise ValueError(f"candidate manifest has an invalid hash for {name}")
        if sha256_file(checked_file(root, name)) != recorded:
            raise ValueError(f"candidate file failed its SHA-256 check: {name}")


def parse_manifest(path: Path) -> dict:
    """Read one JSON object manifest from a regular bounded file."""

    if not path.is_file() or path.is_symlink() or path.stat().st_size > 65536:
        raise ValueError("candidate manifest is missing, linked, or oversized")
    document = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(document, dict):
        raise ValueError("candidate manifest must be a JSON object")
    return document


def parser() -> argparse.ArgumentParser:
    """Build the command-line parser shared by producer and consumer jobs."""

    command = argparse.ArgumentParser(description=__doc__)
    command.add_argument("mode", choices=("create", "verify"))
    command.add_argument("--root", type=Path, required=True)
    command.add_argument("--manifest", type=Path, required=True)
    command.add_argument(
        "--kind",
        choices=("local-artifacts", "macos-signing-input"),
        required=True,
    )
    command.add_argument("--repository", required=True)
    command.add_argument("--run-id", type=int, required=True)
    command.add_argument("--commit", required=True)
    command.add_argument("--target", choices=tuple(PROFILES), required=True)
    command.add_argument("--flake-lock", type=Path, required=True)
    return command


def main() -> int:
    """Create or verify one candidate manifest, reporting bounded errors."""

    arguments = parser().parse_args()
    try:
        if arguments.mode == "create":
            manifest = create_manifest(
                root=arguments.root,
                kind=arguments.kind,
                repository=arguments.repository,
                run_id=arguments.run_id,
                commit=arguments.commit,
                target=arguments.target,
                flake_lock=arguments.flake_lock,
            )
            arguments.manifest.write_text(
                json.dumps(manifest, indent=2, sort_keys=False) + "\n", encoding="utf-8"
            )
        else:
            verify_manifest(
                parse_manifest(arguments.manifest),
                root=arguments.root,
                kind=arguments.kind,
                repository=arguments.repository,
                run_id=arguments.run_id,
                commit=arguments.commit,
                target=arguments.target,
                flake_lock=arguments.flake_lock,
            )
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"release candidate verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
