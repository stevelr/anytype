#!/usr/bin/env python

import contextlib
import json
import os
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest import mock


# Wall-clock ceiling for one anyr invocation. Generous enough for a live
# server, but bounded so an unresponsive endpoint cannot wedge the suite and
# skip a test's cleanup.
CLI_TIMEOUT_SECONDS = 180


def anyr_bin() -> str | None:
    return os.environ.get("ANYR_BIN") or shutil.which("anyr")


def base_env() -> dict:
    env = os.environ.copy()
    # test environment settings override default
    test_key_path = env.get("ANYTYPE_TEST_KEY_FILE")
    if test_key_path:
        env["ANYTYPE_KEY_FILE"] = test_key_path
    test_url = env.get("ANYTYPE_TEST_URL")
    if test_url:
        env["ANYTYPE_URL"] = test_url
    return env


def run_help(*args: str) -> subprocess.CompletedProcess[str]:
    cmd = [anyr_bin(), *args, "--help"]
    return subprocess.run(
        cmd,
        check=False,
        capture_output=True,
        text=True,
        env=base_env(),
        timeout=CLI_TIMEOUT_SECONDS,
    )


def run_anyr(*args: str) -> subprocess.CompletedProcess[str]:
    cmd = [anyr_bin(), *args]
    print(f"running cmd: {cmd}")
    return subprocess.run(
        cmd,
        check=False,
        capture_output=True,
        text=True,
        env=base_env(),
        timeout=CLI_TIMEOUT_SECONDS,
    )


def run_anyr_with_input(
    input_text: str, *args: str
) -> subprocess.CompletedProcess[str]:
    cmd = [anyr_bin(), *args]
    print(f"running cmd with stdin: {cmd}")
    return subprocess.run(
        cmd,
        check=False,
        capture_output=True,
        text=True,
        input=input_text,
        env=base_env(),
        timeout=CLI_TIMEOUT_SECONDS,
    )


def run_anyr_json(*args: str) -> dict:
    result = run_anyr(*args, "--json")
    if result.returncode != 0:
        raise AssertionError(
            f"command failed: {' '.join(args)}\nstdout: {result.stdout}\nstderr: {result.stderr}"
        )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise AssertionError(
            f"invalid json for {' '.join(args)}: {exc}\nstdout: {result.stdout}\n"
            f"stderr: {result.stderr}"
        ) from exc


def wait_for_space_absence(
    space_name: str, space_id: str, timeout_seconds: float = 30
) -> None:
    """Wait until `space get` reports the explicit not-found outcome."""
    deadline = time.monotonic() + timeout_seconds
    expected_error = f'space "{space_id}" was not found'
    while True:
        result = run_anyr("space", "get", space_id)
        if result.returncode != 0:
            error_lines = [line.strip() for line in result.stderr.splitlines()]
            if result.returncode == 1 and expected_error in error_lines:
                return
            raise AssertionError(
                f"failed to verify deletion of disposable space {space_name}:\n"
                f"stdout={result.stdout}\nstderr={result.stderr}"
            )
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"disposable space {space_name} still resolves after deletion"
            )
        time.sleep(1)


class TestDisposableSpaceCleanup(unittest.TestCase):
    @staticmethod
    def get_result(stderr: str, returncode: int = 1) -> subprocess.CompletedProcess:
        return subprocess.CompletedProcess(
            args=["anyr", "space", "get", "space-id"],
            returncode=returncode,
            stdout="",
            stderr=stderr,
        )

    def test_explicit_not_found_proves_absence(self) -> None:
        result = self.get_result(
            'space "space-id" was not found\n'
            "  hint: run `anyr space list -t` to see the spaces you can access\n"
        )
        with mock.patch(__name__ + ".run_anyr", return_value=result):
            wait_for_space_absence("owned-space", "space-id", timeout_seconds=0)

    def test_diagnostics_before_not_found_still_prove_absence(self) -> None:
        result = self.get_result(
            "DEBUG anytype::client: creating HTTP client\n"
            'space "space-id" was not found\n'
            "  hint: run `anyr space list -t` to see the spaces you can access\n"
        )
        with mock.patch(__name__ + ".run_anyr", return_value=result):
            wait_for_space_absence("owned-space", "space-id", timeout_seconds=0)

    def test_transport_failure_does_not_prove_absence(self) -> None:
        result = self.get_result(
            "HTTP transport error for GET /v1/spaces/space-id: connection refused\n"
        )
        with (
            mock.patch(__name__ + ".run_anyr", return_value=result),
            self.assertRaisesRegex(AssertionError, "HTTP transport error"),
        ):
            wait_for_space_absence("owned-space", "space-id", timeout_seconds=0)

    def test_server_failure_does_not_prove_absence(self) -> None:
        result = self.get_result(
            "Anytype API error 500 for GET /v1/spaces/space-id: internal error\n"
        )
        with (
            mock.patch(__name__ + ".run_anyr", return_value=result),
            self.assertRaisesRegex(AssertionError, "Anytype API error 500"),
        ):
            wait_for_space_absence("owned-space", "space-id", timeout_seconds=0)


class TestAnyrCommands(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not anyr_bin():
            raise unittest.SkipTest(
                "anyr binary not found; set ANYR_BIN or add to PATH"
            )
        prefix = os.environ.get("ANYTYPE_TEST_SPACE_PREFIX")
        if not prefix:
            raise unittest.SkipTest("ANYTYPE_TEST_SPACE_PREFIX is not set")
        if len(prefix) > 485 or not all(
            char.isascii() and (char.isalnum() or char in "-_") for char in prefix
        ):
            raise unittest.SkipTest("ANYTYPE_TEST_SPACE_PREFIX is invalid")
        cls.space_prefix = prefix

    def assert_help_ok(self, *args: str) -> None:
        result = run_help(*args)
        self.assertEqual(
            result.returncode,
            0,
            msg=f"help failed for {' '.join(args)}: {result.stderr.strip()}",
        )

    def test_top_level(self) -> None:
        self.assert_help_ok()

    def test_consolidated_cli_surfaces(self) -> None:
        self.assert_help_ok("md")
        self.assert_help_ok("md", "get")
        self.assert_help_ok("md", "update")
        self.assert_help_ok("md", "edit")
        self.assert_help_ok("backup")
        for command in (
            "create",
            "restore",
            "list",
            "manifest",
            "diff",
            "extract",
            "export",
            "import",
        ):
            self.assert_help_ok("backup", command)
        self.assert_help_ok("mcp")

        version = run_anyr("--version")
        self.assertEqual(version.returncode, 0, msg=version.stderr)
        self.assertTrue(version.stdout.startswith("anyr "))

        nested_version = run_anyr("mcp", "--version")
        self.assertNotEqual(nested_version.returncode, 0)
        self.assertIn("anyr --version", nested_version.stderr)

        with tempfile.TemporaryDirectory() as directory:
            config_path = os.path.join(directory, "policy.toml")
            initialized = run_anyr("mcp", "init", "-c", config_path)
            self.assertEqual(initialized.returncode, 0, msg=initialized.stderr)
            checked = run_anyr("mcp", "check", "-c", config_path)
            self.assertEqual(checked.returncode, 0, msg=checked.stderr)
            duplicate = run_anyr("mcp", "init", "-c", config_path)
            self.assertNotEqual(duplicate.returncode, 0)

    def test_auth(self) -> None:
        self.assert_help_ok("auth")
        self.assert_help_ok("auth", "login")
        self.assert_help_ok("auth", "logout")
        self.assert_help_ok("auth", "status")
        self.assert_help_ok("auth", "set-http")
        self.assert_help_ok("auth", "set-grpc")

    def test_space(self) -> None:
        self.assert_help_ok("space")
        self.assert_help_ok("space", "list")
        self.assert_help_ok("space", "get")
        self.assert_help_ok("space", "create")
        self.assert_help_ok("space", "update")
        self.assert_help_ok("space", "delete")
        self.assert_help_ok("space", "invite")
        self.assert_help_ok("space", "invite", "show")
        self.assert_help_ok("space", "invite", "create")
        self.assert_help_ok("space", "invite", "revoke")
        self.assert_help_ok("space", "enable-sharing")
        self.assert_help_ok("space", "disable-sharing")

    def test_space_delete_accepts_bash_stdin_confirmation(self) -> None:
        space_name = f"{self.space_prefix}-delete-stdin-{int(time.time() * 1000)}"
        created_space_id: str | None = None
        deleted = False
        try:
            created = run_anyr_json("space", "create", space_name)
            created_space_id = created.get("id")
            self.assertIsInstance(created_space_id, str)

            wrong_confirmation = run_anyr_with_input(
                f'n\ndelete:{space_name}-wrong\n',
                "space",
                "delete",
                created_space_id,
            )
            self.assertEqual(
                wrong_confirmation.returncode,
                0,
                msg=(
                    "wrong deletion confirmation should cancel without an error:\n"
                    f"stdout={wrong_confirmation.stdout}\n"
                    f"stderr={wrong_confirmation.stderr}"
                ),
            )
            self.assertEqual(
                run_anyr("space", "get", created_space_id).returncode,
                0,
                "space still needs to exist after a wrong confirmation",
            )

            confirmation = run_anyr_with_input(
                f'n\ndelete:{space_name}\n',
                "space",
                "delete",
                created_space_id,
                "--json",
            )
            self.assertEqual(
                confirmation.returncode,
                0,
                msg=(
                    "exact stdin confirmation should delete the space:\n"
                    f"stdout={confirmation.stdout}\n"
                    f"stderr={confirmation.stderr}"
                ),
            )
            wait_for_space_absence(space_name, created_space_id)
            deleted = True
        finally:
            if created_space_id and not deleted:
                cleanup = run_anyr_with_input(
                    f'n\ndelete:{space_name}\n',
                    "space",
                    "delete",
                    created_space_id,
                )
                try:
                    wait_for_space_absence(space_name, created_space_id)
                except AssertionError as exc:
                    raise AssertionError(
                        f"failed to clean up disposable space {space_name}:\n"
                        f"delete stdout={cleanup.stdout}\n"
                        f"delete stderr={cleanup.stderr}"
                    ) from exc

    def test_object(self) -> None:
        self.assert_help_ok("object")
        self.assert_help_ok("object", "list")
        self.assert_help_ok("object", "get")
        self.assert_help_ok("object", "create")
        self.assert_help_ok("object", "update")
        self.assert_help_ok("object", "delete")

    def test_file(self) -> None:
        self.assert_help_ok("file")
        self.assert_help_ok("file", "list")
        self.assert_help_ok("file", "search")
        self.assert_help_ok("file", "get")
        self.assert_help_ok("file", "update")
        self.assert_help_ok("file", "delete")
        self.assert_help_ok("file", "download")
        self.assert_help_ok("file", "upload")

    @contextlib.contextmanager
    def disposable_space(self, label: str):
        """Yield a freshly created prefix-owned space id, deleting it on exit.

        The space is created solely for this test, so every object it holds is
        removed with it and no ambient space is mutated.
        """
        space_name = f"{self.space_prefix}-{label}-{int(time.time() * 1000)}"
        created = run_anyr_json("space", "create", space_name)
        space_id = created.get("id")
        self.assertIsInstance(space_id, str, "space create missing id")
        try:
            yield space_id
        finally:
            deleted = run_anyr_with_input(
                f"n\ndelete:{space_name}\n", "space", "delete", space_id
            )
            self.assertEqual(
                deleted.returncode,
                0,
                msg=(
                    f"failed to delete disposable space {space_name}:\n"
                    f"stdout={deleted.stdout}\nstderr={deleted.stderr}"
                ),
            )
            # Prove the deletion landed before returning. Other tests select
            # their working space by prefix, so a space that lingers in the
            # listing would silently be adopted by one of them. Only the CLI's
            # explicit not-found outcome proves absence; transport and server
            # failures must fail cleanup rather than masquerade as deletion.
            wait_for_space_absence(space_name, space_id)

    def test_file_operations(self) -> None:
        """Server-backed coverage for the migrated REST-default file surface."""
        self.assert_help_ok("file", "metadata")
        self.assert_help_ok("file", "preload")
        self.assert_help_ok("file", "discard-preload")

        payload = b"anyr file surface coverage\n" * 16
        with (
            self.disposable_space("file") as space_id,
            tempfile.TemporaryDirectory() as work,
        ):
            source = os.path.join(work, "coverage.txt")
            with open(source, "wb") as handle:
                handle.write(payload)

            # Plain path upload: REST backend, no gRPC-only detail struct.
            uploaded = run_anyr_json(
                "file", "upload", space_id, "-f", source, "--mime", "text/plain"
            )
            file_id = uploaded.get("id")
            self.assertIsInstance(file_id, str, "file upload missing id")
            self.assertEqual(uploaded.get("size"), len(payload))
            self.assertIsNone(
                uploaded.get("details"), "path upload must use the REST backend"
            )

            fetched = run_anyr_json("file", "get", space_id, file_id)
            self.assertEqual(fetched.get("id"), file_id, "file get id mismatch")

            listed = run_anyr_json("file", "list", space_id, "--limit", "100")
            self.assertTrue(
                any(item.get("id") == file_id for item in listed.get("items", [])),
                "file list is missing the uploaded file",
            )

            # search --sort / --sort --desc must both succeed and stay scoped.
            for extra in (["--sort", "name"], ["--sort", "name", "--desc"]):
                found = run_anyr_json(
                    "file", "search", space_id, "--limit", "100", *extra
                )
                self.assertTrue(
                    any(item.get("id") == file_id for item in found.get("items", [])),
                    f"file search {' '.join(extra)} is missing the uploaded file",
                )
            self.assertNotEqual(
                run_anyr("file", "search", space_id, "--desc", "--json").returncode,
                0,
                "--desc requires --sort",
            )

            # HEAD metadata carries the length and no body.
            meta = run_anyr_json("file", "metadata", space_id, file_id)
            self.assertEqual(meta.get("status"), 200)
            metadata = meta.get("metadata", {})
            self.assertEqual(metadata.get("content_length"), len(payload))

            # Full REST download writes the exact bytes.
            target = os.path.join(work, "downloaded.txt")
            downloaded = run_anyr_json(
                "file", "download", space_id, file_id, "-f", target
            )
            self.assertEqual(downloaded.get("status"), 200)
            self.assertTrue(downloaded.get("written"))
            with open(target, "rb") as handle:
                self.assertEqual(handle.read(), payload)

            self.assertEqual(metadata.get("accept_ranges"), "bytes")

            # A ranged request writes only the requested window.
            partial = os.path.join(work, "partial.txt")
            ranged = run_anyr_json(
                "file",
                "download",
                space_id,
                file_id,
                "-f",
                partial,
                "--range",
                "bytes=0-9",
            )
            self.assertEqual(ranged.get("status"), 206)
            self.assertTrue(ranged.get("written"))
            self.assertEqual(
                ranged.get("metadata", {}).get("content_range"),
                f"bytes 0-9/{len(payload)}",
            )
            with open(partial, "rb") as handle:
                self.assertEqual(handle.read(), payload[:10])

            # 416 and 412 must leave the destination untouched. The server
            # supplies no ETag/Last-Modified, so 304 is not reachable here
            # (tracked as any-5pkh); an unmatchable If-Match still exercises
            # the non-writing branch.
            for extra, expected in (
                (
                    ["--range", f"bytes={len(payload) + 4096}-{len(payload) + 4100}"],
                    416,
                ),
                (["--if-match", '"anyr-never-matches"'], 412),
            ):
                sentinel = os.path.join(work, f"sentinel-{expected}.txt")
                with open(sentinel, "wb") as handle:
                    handle.write(b"sentinel")
                refused = run_anyr_json(
                    "file", "download", space_id, file_id, "-f", sentinel, *extra
                )
                self.assertEqual(refused.get("status"), expected)
                self.assertFalse(refused.get("written"))
                with open(sentinel, "rb") as handle:
                    self.assertEqual(handle.read(), b"sentinel")

            # An unmatchable cache validator still yields the complete file.
            revalidated = run_anyr_json(
                "file",
                "download",
                space_id,
                file_id,
                "-f",
                os.path.join(work, "revalidated.txt"),
                "--if-none-match",
                '"anyr-never-matches"',
            )
            self.assertEqual(revalidated.get("status"), 200)
            self.assertEqual(revalidated.get("bytes"), len(payload))

            # A gRPC-only option promotes a path upload to the gRPC backend.
            # File objects are content addressed, so the promoted upload needs
            # distinct bytes or it would resolve to the REST upload's object.
            rich_source = os.path.join(work, "coverage-rich.txt")
            with open(rich_source, "wb") as handle:
                handle.write(payload + b"rich\n")
            rich = run_anyr_json(
                "file",
                "upload",
                space_id,
                "-f",
                rich_source,
                "--file-type",
                "file",
                "--style",
                "embed",
            )
            rich_id = rich.get("id")
            self.assertIsInstance(rich_id, str, "rich upload missing id")
            self.assertNotEqual(rich_id, file_id, "distinct bytes must not dedupe")
            self.assertIsInstance(
                rich.get("details"), dict, "a rich option must select gRPC"
            )
            self.assertNotEqual(
                run_anyr(
                    "file",
                    "upload",
                    space_id,
                    "-f",
                    rich_source,
                    "--file-type",
                    "file",
                    "--mime",
                    "text/plain",
                ).returncode,
                0,
                "REST-only --mime must be rejected under a gRPC promotion",
            )

            # Preload round trip: the reservation is always discarded again.
            preloaded = run_anyr_json("file", "preload", space_id, "-f", source)
            preload_id = preloaded.get("preload_file_id")
            self.assertIsInstance(preload_id, str, "preload missing id")
            discarded = run_anyr_json("file", "discard-preload", space_id, preload_id)
            self.assertTrue(discarded.get("discarded"))

            # Bin delete and permanent delete both go through the REST builder.
            binned = run_anyr_json("file", "delete", space_id, file_id)
            self.assertTrue(binned.get("deleted"))
            self.assertFalse(binned.get("permanent"))
            purged = run_anyr_json("file", "delete", space_id, rich_id, "--permanent")
            self.assertTrue(purged.get("deleted"))
            self.assertTrue(purged.get("permanent"))

    def test_type(self) -> None:
        self.assert_help_ok("type")
        self.assert_help_ok("type", "list")
        self.assert_help_ok("type", "get")
        self.assert_help_ok("type", "create")
        self.assert_help_ok("type", "update")
        self.assert_help_ok("type", "delete")

    def test_property(self) -> None:
        self.assert_help_ok("property")
        self.assert_help_ok("property", "list")
        self.assert_help_ok("property", "get")
        self.assert_help_ok("property", "create")
        self.assert_help_ok("property", "update")
        self.assert_help_ok("property", "delete")

    def test_member(self) -> None:
        self.assert_help_ok("member")
        self.assert_help_ok("member", "list")
        self.assert_help_ok("member", "get")

    def test_tag(self) -> None:
        self.assert_help_ok("tag")
        self.assert_help_ok("tag", "list")
        self.assert_help_ok("tag", "get")
        self.assert_help_ok("tag", "create")
        self.assert_help_ok("tag", "update")
        self.assert_help_ok("tag", "delete")

    def test_template(self) -> None:
        self.assert_help_ok("template")
        self.assert_help_ok("template", "list")
        self.assert_help_ok("template", "get")

    def test_search(self) -> None:
        self.assert_help_ok("search")

    def test_list(self) -> None:
        self.assert_help_ok("list")
        self.assert_help_ok("list", "objects")
        self.assert_help_ok("list", "views")
        self.assert_help_ok("list", "add")
        self.assert_help_ok("list", "remove")

    def test_real_operations(self) -> None:
        spaces = run_anyr_json("space", "list", "--limit", "200").get("items", [])
        prefix = self.space_prefix.casefold()
        matches = [
            item
            for item in spaces
            if isinstance(item.get("name"), str)
            and item["name"][: len(self.space_prefix)].casefold() == prefix
        ]
        if len(matches) != 1:
            self.skipTest(
                "real operations require exactly one current "
                "ANYTYPE_TEST_SPACE_PREFIX-matching space"
            )
        space_id = matches[0].get("id")
        if not isinstance(space_id, str) or not space_id:
            self.fail("prefix-matching space is missing an id")
        space = run_anyr_json("space", "get", space_id)
        space_name = space.get("name")
        suffix = str(int(time.time() * 1000))
        type_key = f"cli_test_type_{suffix}"
        type_name = f"CLI Test Type {suffix}"
        prop_key = f"cli_test_status_{suffix}"
        prop_name = f"CLI Test Status {suffix}"
        type_prop_key = f"note_{suffix}"
        obj_name = f"CLI Test Object {suffix}"
        updated_obj_name = f"{obj_name} Updated"
        tag_key = f"doing_{suffix}"

        created_type_id = None
        created_prop_id = None
        created_tag_id = None
        created_obj_id = None

        try:
            if (
                space_name
                and len([item for item in spaces if item.get("name") == space_name])
                != 1
            ):
                space_name = None

            typ = run_anyr_json(
                "type",
                "create",
                space_id,
                type_key,
                type_name,
                "-p",
                f"{type_prop_key}:text:Note",
            )
            created_type_id = typ.get("id")
            self.assertIsNotNone(created_type_id, "type create missing id")

            type_by_key = run_anyr_json("type", "get", space_id, type_key)
            self.assertEqual(
                type_by_key.get("id"), created_type_id, "type get by key mismatch"
            )

            type_by_name = run_anyr_json("type", "get", space_id, type_name)
            self.assertEqual(
                type_by_name.get("id"), created_type_id, "type get by name mismatch"
            )

            updated_type = run_anyr_json(
                "type",
                "update",
                space_id,
                type_key,
                "--name",
                f"{type_name} Updated",
            )
            self.assertEqual(
                updated_type.get("id"), created_type_id, "type update by key mismatch"
            )

            prop = run_anyr_json(
                "property",
                "create",
                space_id,
                prop_name,
                "select",
                "--key",
                prop_key,
                "--tag",
                "Todo:blue",
            )
            created_prop_id = prop.get("id")
            self.assertIsNotNone(created_prop_id, "property create missing id")

            prop_by_key = run_anyr_json("property", "get", space_id, prop_key)
            self.assertEqual(
                prop_by_key.get("id"), created_prop_id, "property get by key mismatch"
            )

            updated_prop = run_anyr_json(
                "property",
                "update",
                space_id,
                prop_key,
                "--name",
                f"{prop_name} Updated",
            )
            self.assertEqual(
                updated_prop.get("id"),
                created_prop_id,
                "property update by key mismatch",
            )

            tag = run_anyr_json(
                "tag",
                "create",
                space_id,
                prop_key,
                "Doing",
                "yellow",
                "--key",
                tag_key,
            )
            created_tag_id = tag.get("id")
            self.assertIsNotNone(created_tag_id, "tag create missing id")

            tag_by_key = run_anyr_json("tag", "get", space_id, prop_key, tag_key)
            self.assertEqual(
                tag_by_key.get("id"), created_tag_id, "tag get by key mismatch"
            )

            updated_tag = run_anyr_json(
                "tag",
                "update",
                space_id,
                prop_key,
                tag_key,
                "--name",
                "Done",
            )
            self.assertEqual(
                updated_tag.get("id"), created_tag_id, "tag update by key mismatch"
            )

            obj = run_anyr_json(
                "object",
                "create",
                "--name",
                obj_name,
                "--body",
                "# hello world",
                "-p",
                f"{type_prop_key}=123_text_data",
                space_name or space_id,
                f"@{type_key}",
            )
            created_obj_id = obj.get("id")
            self.assertIsNotNone(created_obj_id, "object create missing id")

            updated_obj = run_anyr_json(
                "object",
                "update",
                space_id,
                created_obj_id,
                "--name",
                updated_obj_name,
            )
            self.assertEqual(
                updated_obj.get("id"), created_obj_id, "object update mismatch"
            )

            list_by_key = run_anyr_json(
                "object",
                "list",
                "--type",
                type_key,
                "--limit",
                "200",
                space_name or space_id,
            )
            items_by_key = list_by_key.get("items", [])
            self.assertTrue(
                any(item.get("id") == created_obj_id for item in items_by_key),
                "object list by type key missing created object",
            )

            list_by_id = run_anyr_json(
                "object", "list", space_id, "--type", created_type_id, "--limit", "200"
            )
            items_by_id = list_by_id.get("items", [])
            self.assertTrue(
                any(item.get("id") == created_obj_id for item in items_by_id),
                "object list by type id missing created object",
            )

            run_anyr_json("template", "list", space_id, "@page")

        finally:
            if created_obj_id:
                run_anyr("object", "delete", space_id, created_obj_id)
            if created_tag_id:
                run_anyr("tag", "delete", space_id, prop_key, tag_key)
            if created_prop_id:
                run_anyr("property", "delete", space_id, prop_key)
            if created_type_id:
                run_anyr("type", "delete", space_id, type_key)


if __name__ == "__main__":
    unittest.main()
