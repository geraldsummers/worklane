"""Complete userspace seed, persistence, backups and settings without npm/network."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
INSTALLER = ROOT / 'assets/worklane-pi-install'


class PiInstallTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.home = root / 'home'
        self.home.mkdir()
        self.seed = root / 'seed'
        self.assets = root / 'assets'
        self.assets.mkdir()
        for name in ('collab.ts', 'p0.ts', 'complain.ts', 'plans.ts', 'questionnaire.ts', 'subscription.ts', 'notifications.ts'):
            (self.assets / name).write_text('export default function() {}\n')
        for package in ('@earendil-works/pi-coding-agent', 'pi-web-access', '@narumitw/pi-plan-mode', 'pi-lightpanda-extension'):
            folder = self.seed / package
            folder.mkdir(parents=True)
            (folder / 'package.json').write_text(json.dumps({'name': package, 'version': 'test'}))
        core = self.seed / '@earendil-works/pi-coding-agent/dist/bundle'
        core.mkdir(parents=True)
        (core / 'cli.js').write_text("#!/bin/sh\nprintf 'test-version\\n'\n")
        (core / 'cli.js').chmod(0o755)
        (self.seed / 'pi-web-access/dist').mkdir()
        (self.seed / '@narumitw/pi-plan-mode/dist').mkdir()
        (self.seed / '@narumitw/pi-plan-mode/dist/index.ts').write_text('export default function() {}\n')
        (self.seed / 'pi-lightpanda-extension/index.ts').write_text('export default function() {}\n')
        (self.seed / 'dependency').mkdir()
        (self.seed / 'dependency/index.js').write_text('module.exports = true;\n')
        self.env = {**os.environ, 'HOME': str(self.home), 'WORKLANE_PI_SEED_ROOT': str(self.seed), 'WORKLANE_PI_ASSETS': str(self.assets)}

    def tearDown(self):
        self.tmp.cleanup()

    def install(self, *args, check=True):
        return subprocess.run([str(INSTALLER), *args], env=self.env, check=check, capture_output=True, text=True)

    def test_seed_repeatable_preserves_edits_and_copies_hoisted_dependencies(self):
        self.install()
        core = self.home / '.local/lib/node_modules/@earendil-works/pi-coding-agent'
        (core / 'lane-change').write_text('preserve')
        (self.seed / '@earendil-works/pi-coding-agent/new-image-file').write_text('not copied')
        extension = self.home / '.pi/agent/extensions/worklane/collab.ts'
        extension.write_text('local customization')
        self.install()
        self.assertEqual((core / 'lane-change').read_text(), 'preserve')
        self.assertFalse((core / 'new-image-file').exists())
        self.assertEqual(extension.read_text(), 'local customization')
        self.assertTrue((self.home / '.local/bin/pi').is_symlink())
        self.assertTrue((self.home / '.local/share/worklane/pi-bundle/node_modules/dependency/index.js').is_file())
        web = self.home / '.local/lib/node_modules/pi-web-access'
        web.unlink()
        self.install()
        self.assertTrue(web.is_dir())

    def test_replace_backs_up_and_preserves_auth_sessions_config(self):
        self.install()
        agent = self.home / '.pi/agent'
        (agent / 'auth.json').write_text('{"test":"private"}')
        (agent / 'sessions').mkdir()
        (agent / 'sessions/example.jsonl').write_text('session data')
        settings = json.loads((agent / 'settings.json').read_text())
        settings['customKey'] = 'preserve'
        settings['defaultProvider'] = 'openrouter'
        settings['defaultModel'] = 'legacy-model'
        (agent / 'settings.json').write_text(json.dumps(settings))
        (self.seed / '@earendil-works/pi-coding-agent/new-release').write_text('new')
        self.install('--replace')
        backup = Path((self.home / '.local/share/worklane/pi-last-backup').read_text().strip())
        self.assertEqual(backup.stat().st_mode & 0o777, 0o700)
        self.assertTrue((backup / 'pi/agent/auth.json').is_file())
        self.assertEqual((agent / 'auth.json').read_text(), '{"test":"private"}')
        self.assertEqual((agent / 'sessions/example.jsonl').read_text(), 'session data')
        restored = json.loads((agent / 'settings.json').read_text())
        self.assertEqual(restored['customKey'], 'preserve')
        self.assertEqual((restored['defaultProvider'], restored['defaultModel'], restored['defaultThinkingLevel']), ('openai-codex', 'gpt-6-sol', 'medium'))
        self.assertTrue((self.home / '.local/lib/node_modules/@earendil-works/pi-coding-agent/new-release').exists())

    def test_resources_local_unique_and_policy_does_not_bypass_shell(self):
        self.install()
        self.install()
        agent = self.home / '.pi/agent'
        settings = json.loads((agent / 'settings.json').read_text())
        self.assertEqual((settings['defaultProvider'], settings['defaultModel'], settings['defaultThinkingLevel']), ('openai-codex', 'gpt-6-sol', 'medium'))
        launcher = (ROOT / 'assets/worklane-pi-agent').read_text()
        self.assertIn('exec "$HOME/.local/bin/pi" "$@"', launcher)
        self.assertNotIn('model_file=', launcher)
        self.assertIn('extensions/worklane/complain.ts', launcher)
        self.assertIn('extensions/worklane/plans.ts', launcher)
        self.assertNotIn('pi-plan-mode', launcher)
        self.assertNotIn('--no-skills', launcher)
        self.assertNotIn('--provider openrouter', launcher)
        self.assertNotIn('OPENROUTER_API_KEY', launcher)
        self.assertEqual(len(settings['packages']), 1)
        self.assertFalse(any('pi-plan-mode' in p or 'pi-lightpanda-extension' in p for p in settings['packages']))
        self.assertEqual(len(settings['extensions']), 7)
        self.assertEqual(settings['enabledModels'], ['openai-codex/*'])
        self.assertFalse(any('npm:' in item for item in settings['packages']))
        self.assertFalse((agent / 'pi-plan-mode.json').exists())
        self.assertTrue(settings['compaction']['enabled'])
        web = json.loads((agent / 'web-search.json').read_text())
        self.assertEqual(web['toolActivation'], 'eager')
        self.assertEqual(web['provider'], 'searxng')
        self.assertEqual(web['webSearch']['allowedProviders'], ['searxng'])
        self.assertNotIn('searchRouting', web)
        self.assertFalse(web['fetchRouting']['allowRemoteHostedProviders'])
        self.assertEqual((agent / 'settings.json').stat().st_mode & 0o777, 0o600)

    def test_repairs_only_obsolete_worklane_strict_search_route(self):
        self.install()
        path = self.home / '.pi/agent/web-search.json'
        web = json.loads(path.read_text())
        web['searchRouting'] = {'providers': ['searxng'], 'fallbackOn': []}
        path.write_text(json.dumps(web))
        self.install()
        self.assertNotIn('searchRouting', json.loads(path.read_text()))
        web['searchRouting'] = {'providers': ['searxng'], 'fallbackOn': ['network']}
        path.write_text(json.dumps(web))
        self.install()
        self.assertEqual(json.loads(path.read_text())['searchRouting'], web['searchRouting'])

    def test_invalid_settings_fail_without_overwriting(self):
        agent = self.home / '.pi/agent'
        agent.mkdir(parents=True)
        settings = agent / 'settings.json'
        settings.write_text('invalid json')
        self.assertNotEqual(self.install(check=False).returncode, 0)
        self.assertEqual(settings.read_text(), 'invalid json')


if __name__ == '__main__':
    unittest.main()
