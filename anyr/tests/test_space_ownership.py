"""Hermetic ownership-boundary tests for the Python live CLI fixtures."""

import subprocess
import unittest
from unittest import mock

from anyr.tests import cli_commands


def page(items, offset=0, has_more=False, total=None):
    """Build the exact paged shape emitted by ``anyr space list --json``."""
    return {
        "items": items,
        "pagination": {
            "has_more": has_more,
            "limit": cli_commands.SPACE_PAGE_SIZE,
            "offset": offset,
            "total": len(items) if total is None else total,
        },
    }


def space(space_id, name, model="space"):
    return {"id": space_id, "name": name, "object": model}


class SpaceOwnershipTests(unittest.TestCase):
    def test_inventory_collects_all_pages_and_rejects_ambient_duplicate(self):
        with mock.patch(
            "anyr.tests.cli_commands.run_anyr_json",
            side_effect=[
                page([space("ambient", "ambient")], has_more=True, total=2),
                page([space("later", "later")], offset=1, total=2),
            ],
        ):
            inventory = cli_commands.complete_space_inventory()
        self.assertEqual(set(inventory), {"ambient", "later"})

        with (
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                return_value=page(
                    [space("duplicate", "one"), space("duplicate", "two")]
                ),
            ),
            self.assertRaisesRegex(AssertionError, "duplicate"),
        ):
            cli_commands.complete_space_inventory()

    def test_inventory_accepts_supported_non_regular_models_only(self):
        with mock.patch(
            "anyr.tests.cli_commands.run_anyr_json",
            return_value=page(
                [space("regular", "Regular"), space("chat", "Chat", "chat")]
            ),
        ):
            inventory = cli_commands.complete_space_inventory()
        self.assertEqual(inventory["chat"].model, "chat")

        with (
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                return_value=page([space("unknown", "Unknown", "other")]),
            ),
            self.assertRaisesRegex(AssertionError, "invalid space identity"),
        ):
            cli_commands.complete_space_inventory()

    def test_inventory_rejects_malformed_pages_and_page_cap(self):
        with (
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                return_value={"items": [], "pagination": {"has_more": False}},
            ),
            self.assertRaisesRegex(AssertionError, "pagination"),
        ):
            cli_commands.complete_space_inventory()

        for key, value in (
            ("limit", True),
            ("offset", 0.0),
            ("total", "0"),
        ):
            invalid = page([])
            invalid["pagination"][key] = value
            with (
                self.subTest(key=key, value=value),
                mock.patch(
                    "anyr.tests.cli_commands.run_anyr_json", return_value=invalid
                ),
                self.assertRaisesRegex(AssertionError, "pagination"),
            ):
                cli_commands.complete_space_inventory()
        with (
            mock.patch.object(cli_commands, "MAX_SPACE_INVENTORY_PAGES", 1),
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                return_value=page([space("only", "only")], has_more=True, total=2),
            ),
            self.assertRaisesRegex(AssertionError, "page limit"),
        ):
            cli_commands.complete_space_inventory()

    def test_missing_or_invalid_create_receipt_reconciles_one_new_exact_name(self):
        with mock.patch(
            "anyr.tests.cli_commands.run_anyr_json",
            side_effect=[
                page([]),
                {},
                page([space("new", "owned")]),
                space("new", "owned"),
            ],
        ):
            self.assertEqual(cli_commands.create_owned_space("owned"), "new")

        with mock.patch(
            "anyr.tests.cli_commands.run_anyr_json",
            side_effect=[
                page([]),
                {"id": "new"},
                space("new", "wrong-name"),
                page([space("new", "owned")]),
                space("new", "owned"),
            ],
        ):
            self.assertEqual(cli_commands.create_owned_space("owned"), "new")

    def test_timeout_and_ambiguity_refuse_deletion_authority(self):
        timeout = subprocess.TimeoutExpired(["anyr", "space", "create"], 1)
        with (
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                side_effect=[
                    page([space("ambient", "owned")]),
                    timeout,
                    page(
                        [
                            space("ambient", "owned"),
                            space("one", "owned"),
                            space("two", "owned"),
                        ]
                    ),
                ],
            ),
            self.assertRaisesRegex(AssertionError, "ambiguous"),
        ):
            cli_commands.create_owned_space("owned")

    def test_delete_requires_two_fresh_exact_identity_checks(self):
        completed = subprocess.CompletedProcess([], 0, "", "")
        with (
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                side_effect=[space("new", "owned"), space("new", "owned")],
            ) as get_space,
            mock.patch(
                "anyr.tests.cli_commands.run_anyr", return_value=completed
            ) as delete,
            mock.patch("anyr.tests.cli_commands.wait_for_space_absence") as absent,
        ):
            cli_commands.delete_owned_space("owned", "new")
        self.assertEqual(get_space.call_count, 2)
        delete.assert_called_once_with(
            "space", "delete", "new", "--skip-archive", "--confirm"
        )
        absent.assert_called_once_with("owned", "new")

        with (
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                return_value=space("new", "not-owned"),
            ),
            mock.patch("anyr.tests.cli_commands.run_anyr") as delete,
            self.assertRaisesRegex(AssertionError, "identity mismatch"),
        ):
            cli_commands.delete_owned_space("owned", "new")
        delete.assert_not_called()

    def test_second_identity_read_drift_refuses_delete(self):
        with (
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                side_effect=[space("new", "owned"), space("new", "drifted")],
            ),
            mock.patch("anyr.tests.cli_commands.run_anyr") as delete,
            self.assertRaisesRegex(AssertionError, "identity mismatch"),
        ):
            cli_commands.delete_owned_space("owned", "new")
        delete.assert_not_called()

    def test_fresh_fixture_proof_rejects_non_regular_space_model(self):
        with (
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_json",
                return_value=space("new", "owned", "chat"),
            ),
            self.assertRaisesRegex(AssertionError, "identity mismatch"),
        ):
            cli_commands.authorize_owned_space_delete("owned", "new")

    def test_delete_authorization_precedes_every_dispatch(self):
        events = []

        def get_space(*_args):
            events.append("guard")
            return space("new", "owned")

        def dispatch(*_args):
            events.append("dispatch")
            return subprocess.CompletedProcess([], 0, "", "")

        with (
            mock.patch("anyr.tests.cli_commands.run_anyr_json", side_effect=get_space),
            mock.patch(
                "anyr.tests.cli_commands.run_anyr_with_input", side_effect=dispatch
            ),
        ):
            cli_commands.run_owned_space_delete(
                "owned", "new", "--skip-archive", input_text="cancel\n"
            )
        self.assertEqual(events, ["guard", "guard", "dispatch"])

    def test_context_cleans_proven_space_after_caller_setup_failure(self):
        case = cli_commands.TestAnyrCommands()
        case.space_prefix = "owned"
        with (
            mock.patch(
                "anyr.tests.cli_commands.create_owned_space", return_value="new"
            ),
            mock.patch("anyr.tests.cli_commands.delete_owned_space") as cleanup,
            self.assertRaisesRegex(RuntimeError, "setup failed"),
        ):
            with case.disposable_space("fixture"):
                raise RuntimeError("setup failed")
        cleanup.assert_called_once()


if __name__ == "__main__":
    unittest.main()
