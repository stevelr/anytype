#!/usr/bin/env python

import json
import os
import shutil
import subprocess
import time
import unittest


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
        cmd, check=False, capture_output=True, text=True, env=base_env()
    )


def run_anyr(*args: str) -> subprocess.CompletedProcess[str]:
    cmd = [anyr_bin(), *args]
    return subprocess.run(
        cmd, check=False, capture_output=True, text=True, env=base_env()
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


class TestAnyrCommands(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if not anyr_bin():
            raise unittest.SkipTest(
                "anyr binary not found; set ANYR_BIN or add to PATH"
            )
        if "ANYTYPE_TEST_SPACE_ID" not in os.environ:
            raise unittest.SkipTest("ANYTYPE_TEST_SPACE_ID is not set")

    def assert_help_ok(self, *args: str) -> None:
        result = run_help(*args)
        self.assertEqual(
            result.returncode,
            0,
            msg=f"help failed for {' '.join(args)}: {result.stderr.strip()}",
        )

    def test_top_level(self) -> None:
        self.assert_help_ok()

    def test_auth(self) -> None:
        self.assert_help_ok("auth")
        self.assert_help_ok("auth", "login")
        self.assert_help_ok("auth", "logout")
        self.assert_help_ok("auth", "status")
        self.assert_help_ok("auth", "token")

    def test_space(self) -> None:
        self.assert_help_ok("space")
        self.assert_help_ok("space", "list")
        self.assert_help_ok("space", "get")
        self.assert_help_ok("space", "create")
        self.assert_help_ok("space", "update")

    def test_object(self) -> None:
        self.assert_help_ok("object")
        self.assert_help_ok("object", "list")
        self.assert_help_ok("object", "get")
        self.assert_help_ok("object", "create")
        self.assert_help_ok("object", "update")
        self.assert_help_ok("object", "delete")

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

    def test_config(self) -> None:
        self.assert_help_ok("config")
        self.assert_help_ok("config", "show")
        self.assert_help_ok("config", "set")
        self.assert_help_ok("config", "reset")

    def test_real_operations(self) -> None:
        space_id = os.environ["ANYTYPE_TEST_SPACE_ID"]
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
                space_id,
                type_key,
                "--name",
                obj_name,
                f"{type_prop_key}=hello",
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
                f"{type_prop_key}=world",
            )
            self.assertEqual(
                updated_obj.get("id"), created_obj_id, "object update mismatch"
            )

            list_by_key = run_anyr_json(
                "object", "list", space_id, "--type", type_key, "--limit", "200"
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

            run_anyr_json("template", "list", space_id, "page")

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
