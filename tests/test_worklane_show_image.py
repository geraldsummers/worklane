import importlib.machinery
import importlib.util
import json
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import threading
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "assets" / "worklane-show-image"
loader = importlib.machinery.SourceFileLoader("worklane_show_image", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)


def png(width=20, height=10):
    return b"\x89PNG\r\n\x1a\n" + b"\x00\x00\x00\rIHDR" + struct.pack(">II", width, height) + b"rest"


class FakeApi:
    def __init__(self, responses):
        self.responses = list(responses)
        self.requests = []
        self.temp = tempfile.TemporaryDirectory(dir=ROOT)
        self.path = Path(self.temp.name) / "herdr.sock"
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(str(self.path))
        self.server.listen()
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        for response in self.responses:
            connection, _ = self.server.accept()
            with connection:
                request = json.loads(connection.makefile("rb").readline())
                self.requests.append(request)
                response = {"id": request["id"], **response}
                connection.sendall((json.dumps(response) + "\n").encode())

    def close(self):
        self.thread.join(timeout=2)
        self.server.close()
        self.temp.cleanup()


class ShowImageTests(unittest.TestCase):
    def test_placement_scales_and_centers_without_distorting(self):
        self.assertEqual(
            module.placement(1600, 800, 100, 40, 8, 16),
            {"viewport_col": 0, "viewport_row": 7, "grid_cols": 100, "grid_rows": 25},
        )

    def test_non_png_is_normalized_with_graphicsmagick(self):
        completed = subprocess.CompletedProcess([], 0, stdout=png(32, 24), stderr=b"")
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            image = Path(directory) / "sample.jpg"
            image.write_bytes(b"jpeg")
            with mock.patch.object(module.subprocess, "run", return_value=completed) as run:
                data, width, height = module.load_png(image)
        self.assertEqual((data, width, height), (completed.stdout, 32, 24))
        run.assert_called_once_with(
            ["gm", "convert", str(image), "png:-"], check=False, capture_output=True
        )

    def test_main_sends_native_graphics_payload_then_focuses(self):
        api = FakeApi(
            [
                {"result": {"type": "pane_graphics_info", "cell_width_px": 8, "cell_height_px": 16}},
                {"result": {"type": "ok"}},
            ]
        )
        calls = []

        def fake_herdr(session, *args):
            calls.append((session, *args))
            if args[:2] == ("workspace", "create"):
                return {
                    "result": {
                        "workspace": {"workspace_id": "w1"},
                        "tab": {"tab_id": "t1"},
                        "root_pane": {"pane_id": "p1"},
                    }
                }
            if args[:2] == ("pane", "layout"):
                return {
                    "result": {
                        "layout": {
                            "panes": [{"pane_id": "p1", "rect": {"width": 80, "height": 24}}]
                        }
                    }
                }
            return {"result": {}}

        try:
            with tempfile.TemporaryDirectory(dir=ROOT) as directory:
                image = Path(directory) / "sample.png"
                image.write_bytes(png())
                with mock.patch.object(module, "run_herdr", side_effect=fake_herdr), mock.patch.dict(
                    module.os.environ,
                    {"HERDR_SOCKET_PATH": str(api.path), "HERDR_SESSION": "lane"},
                    clear=False,
                ):
                    self.assertEqual(module.main(["--session", "lane", str(image)]), 0)
        finally:
            api.close()

        self.assertEqual([request["method"] for request in api.requests], ["pane.graphics.info", "pane.graphics.set"])
        payload = api.requests[1]["params"]
        self.assertEqual((payload["format"], payload["image_width"], payload["image_height"]), ("png", 20, 10))
        self.assertEqual(payload["placement"], {"viewport_col": 38, "viewport_row": 11, "grid_cols": 3, "grid_rows": 1})
        self.assertIn(("lane", "workspace", "focus", "w1"), calls)
        self.assertIn(("lane", "tab", "focus", "t1"), calls)

    def test_graphics_failure_closes_new_workspace(self):
        api = FakeApi([{"error": {"code": "cell_size_unavailable", "message": "no frontend"}}])
        calls = []

        def fake_herdr(session, *args):
            calls.append((session, *args))
            if args[:2] == ("workspace", "create"):
                return {
                    "result": {
                        "workspace": {"workspace_id": "w1"},
                        "tab": {"tab_id": "t1"},
                        "root_pane": {"pane_id": "p1"},
                    }
                }
            return {"result": {}}

        try:
            with tempfile.TemporaryDirectory(dir=ROOT) as directory:
                image = Path(directory) / "sample.png"
                image.write_bytes(png())
                with mock.patch.object(module, "run_herdr", side_effect=fake_herdr), mock.patch.dict(
                    module.os.environ,
                    {"HERDR_SOCKET_PATH": str(api.path), "HERDR_SESSION": "lane"},
                    clear=False,
                ):
                    with self.assertRaisesRegex(module.ShowImageError, "connected Ghostty frontend"):
                        module.main(["--session", "lane", str(image)])
        finally:
            api.close()
        self.assertIn(("lane", "workspace", "close", "w1"), calls)


if __name__ == "__main__":
    unittest.main()
