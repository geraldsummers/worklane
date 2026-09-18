"""Bundle publication contracts; no live Codex sessions or user instructions are edited."""

from concurrent.futures import ThreadPoolExecutor
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "agent_docs", ROOT / "assets/worklane-agent-docs.py"
)
installer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(installer)
DOCUMENTS = {
    name: (ROOT / "docs/agents" / name).read_text()
    for name in ("lane.md", "automation.md", "MIGRATE.md")
}


class AgentDocsTest(unittest.TestCase):
    def setUp(self):
        temporary_root = Path.home() / ".tmp"
        temporary_root.mkdir(exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=temporary_root)
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(self.temporary.name)
        self.root = self.home / ".local/share/worklane/agent-docs"
        self.active = self.home / ".codex/AGENTS.md"

    def install(self, documents=None):
        return installer.install(self.home, DOCUMENTS if documents is None else documents)

    def test_fresh_bundle_and_repeat_are_complete_and_stable(self):
        revision = self.install()
        current = self.root / "current"
        inode = current.lstat().st_ino
        seed = self.active.read_bytes()
        self.assertEqual((current / "REVISION").read_text(), revision + "\n")
        for name, content in DOCUMENTS.items():
            self.assertEqual((current / name).read_text(), content)
        self.assertNotIn("scripts/release-check", self.active.read_text())
        self.assertEqual(self.install(dict(reversed(list(DOCUMENTS.items())))), revision)
        self.assertEqual(current.lstat().st_ino, inode)
        self.assertEqual(self.active.read_bytes(), seed)
        self.assertEqual(list(self.root.glob(".stage-*")), [])

        lane_guide = (current / "lane.md").read_text()
        self.assertIn("nvidia-smi", lane_guide)
        self.assertIn("framework's own availability check", lane_guide)
        self.assertIn("retain a CPU fallback", lane_guide)
        self.assertIn("Do not install or change host GPU drivers", lane_guide)

    def test_new_bundle_preserves_active_rules_and_previous_version(self):
        previous = self.install()
        self.active.write_text("# Custom rules\nUse my SDK.\n")
        changed = {**DOCUMENTS, "automation.md": "Updated automation.\n"}
        revision = self.install(changed)
        self.assertNotEqual(revision, previous)
        self.assertEqual(self.active.read_text(), "# Custom rules\nUse my SDK.\n")
        self.assertEqual((self.root / "current/automation.md").read_text(), changed["automation.md"])
        self.assertEqual((self.root / "versions" / previous / "automation.md").read_text(), DOCUMENTS["automation.md"])

    def test_existing_files_empty_files_and_dangling_links_are_preserved(self):
        for filename in ("AGENTS.md", "AGENTS.override.md"):
            for kind in ("custom", "empty", "symlink", "directory"):
                with self.subTest(filename=filename, kind=kind):
                    home = self.home / filename / kind
                    codex = home / ".codex"
                    codex.mkdir(parents=True)
                    target = codex / filename
                    if kind == "symlink":
                        target.symlink_to("missing-user-file")
                    elif kind == "directory":
                        target.mkdir()
                    else:
                        target.write_text("Custom instructions\n" if kind == "custom" else "")
                    before = target.lstat()
                    installer.install(home, DOCUMENTS)
                    self.assertEqual(target.lstat().st_ino, before.st_ino)
                    if filename == "AGENTS.override.md":
                        self.assertFalse((codex / "AGENTS.md").exists())
                    if kind == "symlink":
                        self.assertEqual(target.readlink(), Path("missing-user-file"))
                    elif kind != "directory":
                        self.assertEqual(target.read_text(), "Custom instructions\n" if kind == "custom" else "")

    def test_failed_publication_leaves_old_current_and_can_retry(self):
        old = self.install()
        seed = self.active.read_bytes()
        changed = {**DOCUMENTS, "lane.md": "New lane guide\n"}
        with patch.object(installer.os, "replace", side_effect=OSError("interrupted")):
            with self.assertRaisesRegex(OSError, "interrupted"):
                self.install(changed)
        self.assertEqual((self.root / "current/REVISION").read_text().strip(), old)
        self.assertEqual(self.active.read_bytes(), seed)
        new = self.install(changed)
        self.assertNotEqual(new, old)
        self.assertEqual((self.root / "current/lane.md").read_text(), "New lane guide\n")

    def test_failed_staging_never_publishes_or_seeds_partial_content(self):
        with patch.object(installer.os, "fsync", side_effect=OSError("disk full")):
            with self.assertRaisesRegex(OSError, "disk full"):
                self.install()
        self.assertFalse((self.root / "current").exists())
        self.assertFalse(self.active.exists())
        self.install()
        self.assertTrue(self.active.exists())

    def test_concurrent_attaches_publish_one_complete_version(self):
        with ThreadPoolExecutor(max_workers=4) as workers:
            revisions = list(workers.map(lambda _: self.install(), range(8)))
        self.assertEqual(len(set(revisions)), 1)
        self.assertEqual(len(list((self.root / "versions").iterdir())), 1)
        self.assertEqual(self.active.read_text().count("Worklane guidance bundle:"), 1)

    def test_modified_managed_reference_is_reported_without_replacing_user_rules(self):
        self.install()
        seed = self.active.read_bytes()
        (self.root / "current/lane.md").write_text("unexpected change")
        with self.assertRaisesRegex(ValueError, "managed agent guidance was modified"):
            self.install()
        self.assertEqual(self.active.read_bytes(), seed)

    def test_concurrent_creator_of_active_file_is_never_overwritten(self):
        original = installer.os.link

        def competing_link(source, destination):
            Path(destination).write_text("Written by another process\n")
            original(source, destination)

        with patch.object(installer.os, "link", side_effect=competing_link):
            self.install()
        self.assertEqual(self.active.read_text(), "Written by another process\n")


if __name__ == "__main__":
    unittest.main()
