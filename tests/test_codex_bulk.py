#!/usr/bin/env python3

import json
from pathlib import Path
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "codex-bulk"
WORK = ROOT / ".tmp" / "codex-bulk-tests"


class CodexBulkTest(unittest.TestCase):
    def setUp(self):
        WORK.mkdir(parents=True, exist_ok=True)
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", str(WORK)], check=True))
        self.schema = WORK / "schema.json"
        self.schema.write_text(
            json.dumps(
                {
                    "type": "object",
                    "properties": {"label": {"type": "string"}},
                    "required": ["label"],
                    "additionalProperties": False,
                }
            )
        )
        self.input = WORK / "input.jsonl"
        self.input.write_text('{"id":"a","text":"one"}\n{"id":"b","text":"two"}\n')
        self.fake = WORK / "codex"
        self.fake.write_text(
            "#!/bin/sh\n"
            "record=$(cat)\n"
            "case \"$record\" in *'\"id\":\"a\"'*) id=a ;; *) id=b ;; esac\n"
            "printf '{\"label\":\"%s\"}\\n' \"$id\"\n"
        )
        self.fake.chmod(0o755)

    def test_runs_jsonl_and_resumes_completed_ids(self):
        output = WORK / "results.jsonl"
        command = [
            str(SCRIPT),
            str(self.input),
            "--output",
            str(output),
            "--schema",
            str(self.schema),
            "--prompt",
            "Classify.",
            "--codex",
            str(self.fake),
            "--initial-workers",
            "2",
        ]
        subprocess.run(command, cwd=ROOT, check=True)
        first = output.read_text().splitlines()
        subprocess.run(command, cwd=ROOT, check=True)
        self.assertEqual(output.read_text().splitlines(), first)
        events = [json.loads(line) for line in first]
        self.assertEqual({event["id"] for event in events}, {"a", "b"})
        self.assertTrue(all(event["status"] == "ok" for event in events))

    def test_rejects_more_than_256_workers(self):
        result = subprocess.run(
            [
                str(SCRIPT),
                str(self.input),
                "--output",
                str(WORK / "results.jsonl"),
                "--schema",
                str(self.schema),
                "--prompt",
                "Classify.",
                "--max-workers",
                "257",
            ],
            cwd=ROOT,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("cannot exceed", result.stderr)

    def test_rate_limit_is_persisted_and_retried(self):
        marker = WORK / "rate-limited"
        self.fake.write_text(
            "#!/bin/sh\n"
            f"if [ ! -e '{marker}' ]; then\n"
            f"  touch '{marker}'\n"
            "  echo 'exceeded retry limit, last status: 429 Too Many Requests' >&2\n"
            "  exit 1\n"
            "fi\n"
            "cat >/dev/null\n"
            "printf '{\"label\":\"ok\"}\\n'\n"
        )
        output = WORK / "retry-results.jsonl"
        subprocess.run(
            [
                str(SCRIPT),
                str(self.input),
                "--output",
                str(output),
                "--schema",
                str(self.schema),
                "--prompt",
                "Classify.",
                "--codex",
                str(self.fake),
                "--initial-workers",
                "1",
                "--cooldown",
                "0",
                "--jitter",
                "0",
            ],
            cwd=ROOT,
            check=True,
        )
        events = [json.loads(line) for line in output.read_text().splitlines()]
        self.assertIn("retrying", [event["status"] for event in events])
        self.assertEqual(sum(event["status"] == "ok" for event in events), 2)


if __name__ == "__main__":
    unittest.main()
