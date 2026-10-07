"""Behavior checks for Worklane's lane-owned IDE helpers."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
APPEARANCE = ROOT / "assets/worklane-appearance"
IDE = ROOT / "assets/worklane-ide"


class IdeAssetsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="worklane-ide-")
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.env = {**os.environ, "HOME": str(self.home),
                    "WORKLANE_SESSION": "sample", "WORKLANE_WORKSPACE": str(self.home)}

    def run_asset(self, asset, *args):
        return subprocess.run(["python3", str(asset), *args], env=self.env,
                              text=True, capture_output=True)

    def test_appearance_preserves_other_settings(self):
        pi = self.home / ".pi/agent/settings.json"
        pi.parent.mkdir(parents=True)
        pi.write_text('{"model": "openrouter/example"}')
        codex = self.home / ".codex/config.toml"
        codex.parent.mkdir(parents=True)
        codex.write_text('model = "existing"\n')
        herdr = self.home / ".config/herdr/config.toml"
        herdr.parent.mkdir(parents=True)
        herdr.write_text('[ui]\nwindow_title = "{workspace}"\n')
        for preset in ("warm", "cool"):
            result = self.run_asset(APPEARANCE, preset)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((self.home / ".config/worklane/theme").read_text().strip(), preset)
            self.assertEqual(json.loads(pi.read_text())["model"], "openrouter/example")
            self.assertEqual(json.loads(pi.read_text())["theme"], f"worklane-{preset}")
            self.assertIn('model = "existing"', codex.read_text())
            self.assertIn(f'theme = "worklane-{preset}"', codex.read_text())
            self.assertIn('window_title = "{workspace}"', herdr.read_text())
            self.assertIn("mouse_capture = true", herdr.read_text())

    def test_editor_reuses_tab_and_preserves_focus_by_default(self):
        init = self.home / ".config/worklane/nvim/init.lua"
        init.parent.mkdir(parents=True)
        init.write_text("-- test config\n")
        bin_dir = self.home / "bin"
        bin_dir.mkdir()
        log = self.home / "herdr.log"
        herdr = bin_dir / "herdr"
        herdr.write_text("""#!/usr/bin/env python3
import json,os,pathlib,sys
home=pathlib.Path(os.environ['HOME']); args=sys.argv[3:]
with (home/'herdr.log').open('a') as out: out.write(json.dumps(args)+'\\n')
if args[:2]==['workspace','list']:
 result={'workspaces':[{'workspace_id':'w1','label':'sample'}]}
elif args[:2]==['tab','list']:
 result={'tabs':[{'tab_id':'w1:t2','label':'IDE'}] if (home/'tab-created').exists() else []}
elif args[:2]==['tab','create']:
 (home/'tab-created').touch(); result={'tab':{'tab_id':'w1:t2','label':'IDE'},'root_pane':{'pane_id':'w1:p2'}}
elif args[:2]==['pane','list']:
 result={'panes':[{'pane_id':'w1:p2','tab_id':'w1:t2','label':'IDE'}]}
elif args[:2]==['pane','run']:
 (home/'nvim-ready').touch(); (home/'.local/state/worklane/ide.sock').touch(); result={}
elif args[:2]==['tab','focus']: result={}
else: raise SystemExit('unexpected '+repr(args))
if args[:2] in (['pane','run'], ['tab','focus']): raise SystemExit(0)
print(json.dumps({'result':result}))
""")
        herdr.chmod(0o755)
        nvim = bin_dir / "nvim"
        nvim.write_text("""#!/usr/bin/env python3
import os,pathlib,sys
if '--remote-expr' in sys.argv and not (pathlib.Path(os.environ['HOME'])/'nvim-ready').exists():
 raise SystemExit(1)
print('1')
""")
        nvim.chmod(0o755)
        self.env["PATH"] = str(bin_dir) + os.pathsep + self.env["PATH"]
        for args in (("start",), ("start",), ("focus",)):
            result = self.run_asset(IDE, *args)
            self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(sum(call[:2] == ["tab", "create"] for call in calls), 1)
        self.assertEqual(sum(call[:2] == ["pane", "run"] for call in calls), 1)
        self.assertEqual(sum(call[:2] == ["tab", "focus"] for call in calls), 1)

    def test_task_rejects_workspace_escape(self):
        config = self.home / ".worklane/ide.toml"
        config.parent.mkdir()
        config.write_text('[tasks.escape]\ncommand = "echo unsafe"\ncwd = ".."\n')
        result = self.run_asset(IDE, "task", "escape")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid working directory", result.stderr)


if __name__ == "__main__":
    unittest.main()
