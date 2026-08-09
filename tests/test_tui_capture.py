#!/usr/bin/env python3

import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("tui_capture", ROOT / "scripts" / "tui_capture.py")
TUI_CAPTURE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(TUI_CAPTURE)


class ScreenTest(unittest.TestCase):
    def test_csi_split_across_reads_is_not_rendered_as_text(self):
        screen = TUI_CAPTURE.Screen(rows=3, cols=8)
        screen.feed(b"visible\x1b")
        screen.feed(b"[2;3H!")

        self.assertEqual(screen.grid[0][:7], list("visible"))
        self.assertEqual(screen.grid[1][2], "!")
        self.assertNotIn("[2;3H", screen.text())

    def test_utf8_split_across_reads_decodes_once(self):
        screen = TUI_CAPTURE.Screen(rows=1, cols=4)
        encoded = "│".encode()
        screen.feed(encoded[:1])
        screen.feed(encoded[1:])

        self.assertEqual(screen.grid[0][0], "│")

    def test_osc_split_across_reads_is_not_rendered_as_text(self):
        screen = TUI_CAPTURE.Screen(rows=1, cols=16)
        screen.feed(b"visible\x1b]8;;https://example.com\x1b")
        screen.feed(b"\\linked\x1b]8;;\x1b")
        screen.feed(b"\\!")

        self.assertEqual(screen.text(), "visiblelinked!\n")
        self.assertNotIn("8;;", screen.text())


if __name__ == "__main__":
    unittest.main()
