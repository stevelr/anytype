#!/usr/bin/env python

import contextlib
import dataclasses
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
SPACE_PAGE_SIZE = 200
MAX_SPACE_INVENTORY_PAGES = 16
MAX_SPACE_INVENTORY_ITEMS = SPACE_PAGE_SIZE * MAX_SPACE_INVENTORY_PAGES
SPACE_MODELS = frozenset({"space", "chat", "one_to_one", "tech_space"})


@dataclasses.dataclass(frozen=True)
class SpaceIdentity:
    """A validated space identity from a complete CLI inventory."""

    id: str
    name: str
    model: str


def _inventory_space_identity(record: object, context: str) -> SpaceIdentity:
    if not isinstance(record, dict):
        raise AssertionError(f"{context} is not a space record")
    space_id = record.get("id")
    space_name = record.get("name")
    # Ambient spaces may be unnamed (a fresh account's default space has an
    # empty name); strict naming applies only to the prefix-owned spaces the
    # tests create, which are always selected by exact non-empty name.
    if (
        not isinstance(space_id, str)
        or not space_id
        or not isinstance(space_name, str)
        or record.get("object") not in SPACE_MODELS
    ):
        raise AssertionError(f"{context} has an invalid space identity")
    return SpaceIdentity(space_id, space_name, record["object"])


def complete_space_inventory() -> dict[str, SpaceIdentity]:
    """Return every supported space model after rejecting partial pages."""
    inventory: dict[str, SpaceIdentity] = {}
    offset = 0
    expected_total: int | None = None
    for page_index in range(MAX_SPACE_INVENTORY_PAGES):
        response = run_anyr_json(
            "space", "list", "--limit", str(SPACE_PAGE_SIZE), "--offset", str(offset)
        )
        if not isinstance(response, dict):
            raise AssertionError("space inventory response is invalid")
        items = response.get("items")
        pagination = response.get("pagination")
        if not isinstance(items, list) or not isinstance(pagination, dict):
            raise AssertionError("space inventory pagination is invalid")
        has_more = pagination.get("has_more")
        limit = pagination.get("limit")
        page_offset = pagination.get("offset")
        total = pagination.get("total")
        if (
            not isinstance(has_more, bool)
            or limit != SPACE_PAGE_SIZE
            or page_offset != offset
            or type(limit) is not int
            or type(page_offset) is not int
            or type(total) is not int
            or total < 0
            or len(items) > SPACE_PAGE_SIZE
            or (expected_total is not None and total != expected_total)
        ):
            raise AssertionError("space inventory pagination is invalid")
        expected_total = total
        for item in items:
            identity = _inventory_space_identity(item, "space inventory item")
            if identity.id in inventory:
                raise AssertionError("space inventory contains duplicate ids")
            inventory[identity.id] = identity
            if len(inventory) > MAX_SPACE_INVENTORY_ITEMS:
                raise AssertionError("space inventory exceeds the bounded limit")
        if not has_more:
            if total != len(inventory):
                raise AssertionError("space inventory is incomplete")
            return inventory
        if not items:
            raise AssertionError("space inventory pagination did not advance")
        offset += len(items)
        if offset > MAX_SPACE_INVENTORY_ITEMS:
            raise AssertionError("space inventory exceeds the bounded limit")
    raise AssertionError("space inventory exceeds the bounded page limit")


def _fresh_owned_space(space_id: str, expected_name: str) -> SpaceIdentity:
    record = _inventory_space_identity(
        run_anyr_json("space", "get", space_id), "space get response"
    )
    if record.id != space_id or record.name != expected_name or record.model != "space":
        raise AssertionError("disposable space create identity mismatch")
    return record


def _reconcile_owned_space(
    before: dict[str, SpaceIdentity], expected_name: str
) -> SpaceIdentity:
    after = complete_space_inventory()
    candidates = [
        item
        for space_id, item in after.items()
        if space_id not in before and item.name == expected_name
    ]
    if len(candidates) != 1:
        raise AssertionError("disposable space create ownership is ambiguous")
    candidate = candidates[0]
    return _fresh_owned_space(candidate.id, expected_name)


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


def run_anyr_with_input(input_text: str, *args: str) -> subprocess.CompletedProcess[str]:
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


def create_owned_space(space_name: str) -> str:
    """Create a new exact-name space and prove that it is safe to delete."""
    before = complete_space_inventory()
    try:
        receipt = run_anyr_json("space", "create", space_name)
        if not isinstance(receipt, dict):
            raise AssertionError("disposable space create receipt is invalid")
        candidate = receipt.get("id")
        if not isinstance(candidate, str) or not candidate or candidate in before:
            raise AssertionError("disposable space create did not return a new id")
        return _fresh_owned_space(candidate, space_name).id
    except Exception:
        # A request can commit before its response fails. Reconcile only a
        # complete post-create inventory and only an exact, newly introduced
        # name; ambiguity deliberately leaves all spaces untouched.
        return _reconcile_owned_space(before, space_name).id


def delete_owned_space(space_name: str, space_id: str) -> None:
    """Delete only a twice-revalidated, cleanup-owned regular space."""
    authorize_owned_space_delete(space_name, space_id)
    deleted = run_anyr("space", "delete", space_id, "--skip-archive", "--confirm")
    if deleted.returncode != 0:
        raise AssertionError("disposable space cleanup command failed")
    wait_for_space_absence(space_name, space_id)


def authorize_owned_space_delete(space_name: str, space_id: str) -> None:
    """Require two fresh regular-space identity reads before a delete attempt."""
    for _ in range(2):
        _fresh_owned_space(space_id, space_name)


def run_owned_space_delete(
    space_name: str,
    space_id: str,
    *options: str,
    input_text: str | None = None,
    global_options: tuple[str, ...] = (),
) -> subprocess.CompletedProcess[str]:
    """Authorize an owned space immediately before one delete command dispatch."""
    authorize_owned_space_delete(space_name, space_id)
    args = (*global_options, "space", "delete", space_id, *options)
    if input_text is None:
        return run_anyr(*args)
    return run_anyr_with_input(input_text, *args)


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

    def test_create_owned_space_rejects_ambient_id_and_accepts_exact_identity(
        self,
    ) -> None:
        with mock.patch(
            __name__ + ".run_anyr_json",
            side_effect=[
                {
                    "items": [{"id": "ambient", "name": "old", "object": "space"}],
                    "pagination": {
                        "has_more": False,
                        "limit": 200,
                        "offset": 0,
                        "total": 1,
                    },
                },
                {"id": "new"},
                {"id": "new", "name": "owned", "object": "space"},
            ],
        ):
            self.assertEqual(create_owned_space("owned"), "new")
        with (
            mock.patch(
                __name__ + ".run_anyr_json",
                side_effect=[
                    {
                        "items": [{"id": "ambient", "name": "owned", "object": "space"}],
                        "pagination": {
                            "has_more": False,
                            "limit": 200,
                            "offset": 0,
                            "total": 1,
                        },
                    },
                    {"id": "ambient"},
                    {
                        "items": [{"id": "ambient", "name": "owned", "object": "space"}],
                        "pagination": {
                            "has_more": False,
                            "limit": 200,
                            "offset": 0,
                            "total": 1,
                        },
                    },
                ],
            ),
            self.assertRaisesRegex(AssertionError, "ambiguous"),
        ):
            create_owned_space("owned")

    def test_create_owned_space_reconciliation_refuses_ambiguity(self) -> None:
        with (
            mock.patch(
                __name__ + ".run_anyr_json",
                side_effect=[
                    {
                        "items": [],
                        "pagination": {
                            "has_more": False,
                            "limit": 200,
                            "offset": 0,
                            "total": 0,
                        },
                    },
                    AssertionError("indeterminate"),
                    {
                        "items": [
                            {"id": "one", "name": "owned", "object": "space"},
                            {"id": "two", "name": "owned", "object": "space"},
                        ],
                        "pagination": {
                            "has_more": False,
                            "limit": 200,
                            "offset": 0,
                            "total": 2,
                        },
                    },
                ],
            ),
            self.assertRaisesRegex(AssertionError, "ambiguous"),
        ):
            create_owned_space("owned")

    def test_disposable_space_recovers_missing_create_id_before_cleanup(self) -> None:
        space_name = "owned-receipt-1234000"
        inventory = {
            "items": [],
            "pagination": {
                "has_more": False,
                "limit": 200,
                "offset": 0,
                "total": 0,
            },
        }
        created_inventory = {
            "items": [{"id": "new", "name": space_name, "object": "space"}],
            "pagination": {
                "has_more": False,
                "limit": 200,
                "offset": 0,
                "total": 1,
            },
        }
        case = TestAnyrCommands(methodName="test_top_level")
        case.space_prefix = "owned"
        with (
            mock.patch(
                __name__ + ".run_anyr_json",
                side_effect=[
                    inventory,
                    {"name": "receipt-without-id"},
                    created_inventory,
                    {"id": "new", "name": space_name, "object": "space"},
                ],
            ),
            mock.patch(__name__ + ".time.time", return_value=1234.0),
            mock.patch(__name__ + ".delete_owned_space") as delete,
        ):
            with case.disposable_space("receipt") as space_id:
                self.assertEqual(space_id, "new")
        delete.assert_called_once()
        self.assertEqual(delete.call_args.args[1], "new")


class TestAnyrCommands(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        def unavailable(message: str) -> None:
            if os.environ.get("ANYR_PY_REQUIRE_LIVE") == "1":
                raise AssertionError(message)
            raise unittest.SkipTest(message)

        if not anyr_bin():
            unavailable("anyr binary not found; set ANYR_BIN or add to PATH")
        prefix = os.environ.get("ANYTYPE_TEST_SPACE_PREFIX")
        if not prefix:
            unavailable("ANYTYPE_TEST_SPACE_PREFIX is not set")
        if len(prefix) > 485 or not all(
            char.isascii() and (char.isalnum() or char in "-_") for char in prefix
        ):
            unavailable("ANYTYPE_TEST_SPACE_PREFIX is invalid")
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

    def require_healthy_pings(self) -> None:
        server_log = os.environ.get("ANYBACK_HEADLESS_REDACTED_LOG_FILE")
        self.assertIsNotNone(
            server_log,
            "ANYBACK_HEADLESS_REDACTED_LOG_FILE is required for live deletion evidence",
        )
        self.assertTrue(
            os.path.isabs(server_log) and os.path.isfile(server_log),
            "ANYBACK_HEADLESS_REDACTED_LOG_FILE must name an absolute existing file",
        )
        self.assertGreater(
            os.path.getsize(server_log),
            0,
            "reviewed redacted server log is empty",
        )
        status = run_anyr_json("auth", "status")
        ping = status.get("ping")
        self.assertIsInstance(ping, dict, "auth status is missing ping results")
        for transport in ("http", "grpc"):
            result = ping.get(transport)
            self.assertIsInstance(result, str, f"missing {transport} ping result")
            self.assertIn("ok", result.casefold(), f"{transport} ping is unhealthy")

    @contextlib.contextmanager
    def disposable_deletion_space(self, label: str):
        space_name = f"{self.space_prefix}-{label}-{os.getpid()}-{time.time_ns()}"
        space_id = create_owned_space(space_name)
        state = {"name": space_name, "id": space_id, "deleted": False}
        try:
            yield state
        finally:
            if not state["deleted"]:
                try:
                    delete_owned_space(space_name, space_id)
                except AssertionError as exc:
                    raise AssertionError(
                        f"failed to clean up disposable space {space_name}"
                    ) from exc

    def assert_space_exists(self, space_id: str, reason: str) -> None:
        result = run_anyr("space", "get", space_id)
        self.assertEqual(
            result.returncode,
            0,
            msg=(f"{reason}:\nstdout={result.stdout}\nstderr={result.stderr}"),
        )

    def test_space_delete_prompted_cancellation_and_confirmation(self) -> None:
        self.require_healthy_pings()
        with self.disposable_deletion_space("delete-prompted") as space:
            canceled = run_owned_space_delete(
                space["name"], space["id"], input_text="cancel\n"
            )
            self.assertEqual(canceled.returncode, 0, canceled.stderr)
            self.assertIn("Space deletion canceled.", canceled.stderr)
            self.assert_space_exists(
                space["id"], "archive-choice cancellation deleted the source space"
            )

            wrong_confirmation = run_owned_space_delete(
                space["name"],
                space["id"],
                "--skip-archive",
                input_text=f'delete:{space["name"]}-wrong\n',
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
            self.assert_space_exists(
                space["id"], "wrong typed confirmation deleted the source space"
            )

            confirmation = run_owned_space_delete(
                space["name"],
                space["id"],
                "--skip-archive",
                "--json",
                input_text=f'delete:{space["name"]}\n',
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
            wait_for_space_absence(space["name"], space["id"])
            space["deleted"] = True

    def test_space_delete_backup_failure_preserves_source(self) -> None:
        self.require_healthy_pings()
        with (
            self.disposable_deletion_space("delete-backup-failure") as space,
            tempfile.TemporaryDirectory() as directory,
        ):
            destination = os.path.join(directory, "must-not-exist.zip")
            failed = run_owned_space_delete(
                space["name"],
                space["id"],
                "--archive",
                destination,
                "--confirm",
                global_options=("--grpc", "http://127.0.0.1:1"),
            )
            self.assertNotEqual(
                failed.returncode,
                0,
                msg="unreachable gRPC endpoint unexpectedly produced a backup",
            )
            self.assertIn("deletion was not attempted", failed.stderr)
            self.assertFalse(
                os.path.lexists(destination),
                "failed backup left the selected destination behind",
            )
            self.assert_space_exists(space["id"], "backup failure deleted the source space")

    def test_space_delete_non_interactive_archive_is_exact_and_valid(self) -> None:
        self.require_healthy_pings()
        with (
            self.disposable_deletion_space("delete-archive") as space,
            tempfile.TemporaryDirectory() as directory,
        ):
            decoy = os.path.join(directory, "candidate-old.zip")
            selected = os.path.join(directory, "selected.zip")
            with open(decoy, "wb") as handle:
                handle.write(b"decoy archive candidate")

            deleted = run_owned_space_delete(
                space["name"],
                space["id"],
                "--archive",
                selected,
                "--confirm",
                "--json",
            )
            self.assertEqual(
                deleted.returncode,
                0,
                msg=(
                    "non-interactive backup-before-delete failed:\n"
                    f"stdout={deleted.stdout}\n"
                    f"stderr={deleted.stderr}"
                ),
            )
            result = json.loads(deleted.stdout)
            self.assertTrue(result.get("deleted"))
            self.assertIn(f"Space archived to {selected}.", deleted.stderr)
            self.assertGreater(os.path.getsize(selected), 0)
            with open(decoy, "rb") as handle:
                self.assertEqual(handle.read(), b"decoy archive candidate")

            archive = run_anyr_json("backup", "list", selected, "--files")
            self.assertEqual(archive.get("archive"), selected)
            self.assertEqual(archive.get("source"), "zip")
            self.assertGreater(archive.get("file_count", 0), 0)
            self.assertIsInstance(archive.get("files"), list)

            wait_for_space_absence(space["name"], space["id"])
            space["deleted"] = True

    def test_object(self) -> None:
        self.assert_help_ok("object")
        self.assert_help_ok("object", "list")
        self.assert_help_ok("object", "get")
        self.assert_help_ok("object", "create")
        self.assert_help_ok("object", "update")
        self.assert_help_ok("object", "delete")
        self.assert_help_ok("object", "discussion")
        self.assert_help_ok("object", "discussion", "get")
        self.assert_help_ok("object", "discussion", "attach")

    def test_body(self) -> None:
        self.assert_help_ok("body")
        for command in ("list", "show", "create", "update", "delete", "move"):
            self.assert_help_ok("body", command)

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
        space_id = create_owned_space(space_name)
        try:
            yield space_id
        finally:
            delete_owned_space(space_name, space_id)

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
            self.assertIsNone(uploaded.get("details"), "path upload must use the REST backend")

            fetched = run_anyr_json("file", "get", space_id, file_id)
            self.assertEqual(fetched.get("id"), file_id, "file get id mismatch")

            listed = run_anyr_json("file", "list", space_id, "--limit", "100")
            self.assertTrue(
                any(item.get("id") == file_id for item in listed.get("items", [])),
                "file list is missing the uploaded file",
            )

            # search --sort / --sort --desc must both succeed and stay scoped.
            for extra in (["--sort", "name"], ["--sort", "name", "--desc"]):
                found = run_anyr_json("file", "search", space_id, "--limit", "100", *extra)
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
            downloaded = run_anyr_json("file", "download", space_id, file_id, "-f", target)
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
            self.assertIsInstance(rich.get("details"), dict, "a rich option must select gRPC")
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
        with self.disposable_space("real-operations") as space_id:
            self.assert_real_operations(space_id)

    def test_body_and_attached_discussion_operations(self) -> None:
        """Exercise the verified gRPC body and derived-discussion surfaces."""
        with self.disposable_space("body-discussion") as space_id:
            created = run_anyr_json(
                "object",
                "create",
                space_id,
                "page",
                "--name",
                "Body and discussion diagnostics",
            )
            object_id = created.get("id")
            self.assertIsInstance(object_id, str, "page create missing id")

            absent = run_anyr_json(
                "object", "discussion", "get", space_id, object_id
            )
            self.assertEqual(absent.get("state"), "absent")
            attached = run_anyr_json(
                "object", "discussion", "attach", space_id, object_id
            )
            discussion_id = attached.get("discussion_id")
            self.assertIsInstance(discussion_id, str, "discussion attach missing id")
            repeated = run_anyr_json(
                "object", "discussion", "get", space_id, object_id
            )
            self.assertEqual(repeated.get("discussion_id"), discussion_id)

            initial = run_anyr_json("body", "list", space_id, object_id)
            root_id = initial.get("root_id")
            self.assertIsInstance(root_id, str, "body list missing root id")

            callout_spec = json.dumps(
                {
                    "content": {
                        "kind": "callout",
                        "text": "diagnostic",
                        "icon": {"type": "emoji", "content": "💡"},
                    },
                    "background_color": "grey",
                }
            )
            callout_receipt = run_anyr_json(
                "body",
                "create",
                space_id,
                object_id,
                root_id,
                "last-child",
                "--block",
                callout_spec,
            )
            callout_id = callout_receipt["affected"][0]["block_id"]
            shown = run_anyr_json("body", "show", space_id, object_id, callout_id)
            self.assertEqual(shown.get("parent_id"), root_id)
            self.assertEqual(
                shown.get("content", {}).get("content", {}).get("style"), "callout"
            )

            update_spec = json.dumps(
                {
                    "kind": "text",
                    "text": "updated",
                    "marks": [
                        {
                            "range": {"start": 0, "end": 7},
                            "kind": {"type": "bold"},
                        }
                    ],
                }
            )
            run_anyr_json(
                "body",
                "update",
                space_id,
                object_id,
                callout_id,
                "--change",
                update_spec,
            )

            divider_receipt = run_anyr_json(
                "body",
                "create",
                space_id,
                object_id,
                root_id,
                "last-child",
                "--block",
                json.dumps({"content": {"kind": "divider", "style": "dots"}}),
            )
            divider_id = divider_receipt["affected"][0]["block_id"]
            run_anyr_json(
                "body",
                "move",
                space_id,
                object_id,
                divider_id,
                callout_id,
                "before",
            )
            ordered = run_anyr_json("body", "list", space_id, object_id)
            positions = {
                item["id"]: (item["order"], item["parent_id"], item["sibling_index"])
                for item in ordered.get("items", [])
                if item.get("id") in (divider_id, callout_id)
            }
            self.assertEqual(positions[divider_id][1], root_id)
            self.assertEqual(positions[callout_id][1], root_id)
            self.assertLess(positions[divider_id][0], positions[callout_id][0])
            self.assertLess(positions[divider_id][2], positions[callout_id][2])

            table = run_anyr("body", "list", space_id, object_id, "--table")
            self.assertEqual(table.returncode, 0, table.stderr)
            self.assertIn("parent_id", table.stdout)
            discussion_table = run_anyr(
                "object", "discussion", "get", space_id, object_id, "--table"
            )
            self.assertEqual(discussion_table.returncode, 0, discussion_table.stderr)
            self.assertIn("discussion_id", discussion_table.stdout)

            for block_id in (divider_id, callout_id):
                run_anyr_json(
                    "body",
                    "delete",
                    space_id,
                    object_id,
                    block_id,
                    "--expected-subtree-blocks",
                    "1",
                    "--confirm",
                )

    def assert_real_operations(self, space_id: str) -> None:
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
            self.assertEqual(tag_by_key.get("id"), created_tag_id, "tag get by key mismatch")

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
                space_id,
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
            self.assertEqual(updated_obj.get("id"), created_obj_id, "object update mismatch")

            list_by_key = run_anyr_json(
                "object",
                "list",
                "--type",
                type_key,
                "--limit",
                "200",
                space_id,
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
