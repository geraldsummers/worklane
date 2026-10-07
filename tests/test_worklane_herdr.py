"""Check provenance added to agent prompts by the lane-local Herdr launcher."""

import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "assets/worklane-herdr"
loader = importlib.machinery.SourceFileLoader("worklane_herdr", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)


class HerdrWrapperTests(unittest.TestCase):
    def test_prompt_gets_sender_and_route_from_live_agent_list(self):
        agents = {"result": {"agents": [{"pane_id": "w1:p9", "agent": "codex"}]}}
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps(agents))
        with mock.patch.object(module.subprocess, "run", return_value=completed) as run:
            args = module.prepared_args(
                ["--session", "other", "agent", "prompt", "w2:p1", "Please check src/api.rs"],
                {"HERDR_SESSION": "worklane", "HERDR_PANE_ID": "w1:p9"},
            )
        self.assertEqual(
            args[5],
            "[AGENT MESSAGE]\n"
            "From: codex (Herdr session worklane, pane w1:p9)\n"
            "Reply-to: Herdr session worklane, pane w1:p9\n"
            "Please check src/api.rs",
        )
        run.assert_called_once_with(
            [module.REAL_HERDR, "--session", "worklane", "agent", "list"],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_existing_header_is_replaced_with_verified_identity(self):
        agents = {"result": {"agents": [{"pane_id": "w1:p9", "agent": "codex"}]}}
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps(agents))
        with mock.patch.object(module.subprocess, "run", return_value=completed):
            args = module.prepared_args(
                ["agent", "prompt", "w1:p2", "[AGENT MESSAGE]\nFrom: wrong\nReply-to: wrong\nHello"],
                {"HERDR_SESSION": "worklane", "HERDR_PANE_ID": "w1:p9"},
            )
        self.assertEqual(args[3].count("[AGENT MESSAGE]"), 1)
        self.assertIn("From: codex (Herdr session worklane, pane w1:p9)", args[3])
        self.assertTrue(args[3].endswith("\nHello"))

    def test_agent_prompt_fails_when_sender_is_missing_or_unrecognized(self):
        with self.assertRaisesRegex(ValueError, "needs a Herdr sender pane"):
            module.prepared_args(["agent", "prompt", "w1:p2", "Hello"], {})
        completed = subprocess.CompletedProcess([], 0, stdout=json.dumps({"result": {"agents": []}}))
        with mock.patch.object(module.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(ValueError, "not a recognized agent"):
                module.prepared_args(
                    ["agent", "prompt", "w1:p2", "Hello"],
                    {"HERDR_SESSION": "worklane", "HERDR_PANE_ID": "w1:p9"},
                )

    def test_other_commands_and_explicit_outside_pane_message_pass_through(self):
        self.assertEqual(module.prepared_args(["agent", "list"], {}), ["agent", "list"])
        message = "[AGENT MESSAGE]\nFrom: /root/api\nReply-to: Herdr session lane, pane p1\nHi"
        self.assertEqual(
            module.prepared_args(["agent", "prompt", "p2", message], {}),
            ["agent", "prompt", "p2", message],
        )


if __name__ == "__main__":
    unittest.main()
