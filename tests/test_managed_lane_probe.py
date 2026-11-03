"""Unit-only embedded delivery-program and v3 image contract checks.

The extracted Python runs only against a test-owned directory substituted for
/run. No Podman, systemd, TUIOS, Pi, host auth or model provider is invoked.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'worklane/src/managed_pi.rs'


class ManagedPiDeliveryTests(unittest.TestCase):
    def program(self, runtime):
        source = SOURCE.read_text()
        matches = re.findall(r'const INSTALL: &str = r#"(.*?)"#;', source, re.S)
        self.assertEqual(len(matches), 1)
        program = matches[0]
        self.assertEqual(program.count("os.open('/run',"), 1)
        return program.replace("os.open('/run',", 'os.open(' + repr(str(runtime)) + ',')

    def run_program(self, runtime, key='synthetic-test-key', uid=None):
        # sys.executable avoids any installed lane tooling or wrapper.
        import sys
        return subprocess.run([sys.executable, '-c', self.program(runtime), str(uid if uid is not None else os.getuid() or 1000)],
                              input=json.dumps({'api_key': key}), text=True, capture_output=True,
                              env={'PATH': os.defpath, 'LANG': 'C.UTF-8'}, timeout=5, check=False)

    def test_installs_one_private_provider_file_and_does_not_print_key(self):
        with tempfile.TemporaryDirectory() as temp:
            runtime = Path(temp)
            result = self.run_program(runtime)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout.strip(), 'delivered')
            self.assertNotIn('synthetic-test-key', result.stdout + result.stderr)
            directory = runtime / 'worklane-pi'
            auth = directory / 'auth.json'
            self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
            self.assertEqual(auth.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(auth.read_text()), {'openai': {'type': 'api_key', 'key': 'synthetic-test-key'}})
            again = self.run_program(runtime, key='replacement-key')
            self.assertNotEqual(again.returncode, 0)
            self.assertNotIn('replacement-key', again.stdout + again.stderr)
            self.assertIn('synthetic-test-key', auth.read_text())

    def test_symlink_directory_and_root_recipient_are_rejected_without_delivery(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); runtime = root / 'runtime'; runtime.mkdir()
            external = root / 'external'; external.mkdir()
            (runtime / 'worklane-pi').symlink_to(external, target_is_directory=True)
            self.assertNotEqual(self.run_program(runtime).returncode, 0)
            self.assertEqual(list(external.iterdir()), [])
            (runtime / 'worklane-pi').unlink()
            self.assertNotEqual(self.run_program(runtime, uid=0).returncode, 0)
            self.assertFalse((runtime / 'worklane-pi').exists())

    def test_runtime_root_symlink_is_not_followed(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); external = root / 'external'; external.mkdir()
            link = root / 'runtime'; link.symlink_to(external, target_is_directory=True)
            self.assertNotEqual(self.run_program(link).returncode, 0)
            self.assertEqual(list(external.iterdir()), [])

    def test_v3_image_supervises_native_daemon_without_automatic_agent_replay(self):
        image = (ROOT / 'Containerfile.v3').read_text()
        for text in ('user-runtime-dir@1000.service', 'user@1000.service',
                     'worklane-tuios.service', 'ExecStart=/usr/local/bin/tuios daemon',
                     'KillMode=control-group', 'UMask=0077', 'Restart=on-failure'):
            self.assertIn(text, image)
        for forbidden in ('/usr/local/bin/herdr', 'worklane-herdr.service', 'start-agent', 'OPENAI_API_KEY='):
            self.assertNotIn(forbidden, image)


if __name__ == '__main__':
    unittest.main()
