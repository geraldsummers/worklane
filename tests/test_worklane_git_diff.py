import importlib.machinery
import importlib.util
import json
import multiprocessing
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = Path(os.environ.get("WORKLANE_GIT_DIFF_TEST_ASSET", ROOT / "assets/worklane-git-diff-pane"))
loader = importlib.machinery.SourceFileLoader("worklane_git_diff", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)
MP = multiprocessing.get_context("fork")


def compete(directory, barrier):
    cache = module.Cache(directory, 60)
    barrier.wait(timeout=30)

    def collect():
        with (Path(directory) / "collections").open("a") as handle:
            handle.write("scan\n")
        time.sleep(0.05)
        return ["repository"]

    cache.refresh(["discovery", "root", 4], collect)


def hold_collection(directory, entered):
    def collect():
        entered.set()
        time.sleep(30)
        return []
    module.Cache(directory, 1).refresh(["discovery", "root", 4], collect)


def read_repeatedly(root, cache, barrier, finished):
    watcher = module.Watcher(root, 4, 0.01, cache, cwd=root)
    barrier.wait(timeout=30)
    while not finished.is_set():
        watcher.refresh()
        time.sleep(0.002)


class WatcherTests(unittest.TestCase):
    def setUp(self):
        temporary_root = Path(os.environ.get("TMPDIR", Path.home() / ".tmp"))
        temporary_root.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=temporary_root)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.cache = module.Cache(self.root / "cache", 60)

    def git(self, repo, *args):
        return subprocess.check_output(
            ["git", "-C", str(repo), *args], stderr=subprocess.PIPE,
            env=dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                     GIT_AUTHOR_NAME="Watcher Test", GIT_AUTHOR_EMAIL="watcher@example.test",
                     GIT_COMMITTER_NAME="Watcher Test", GIT_COMMITTER_EMAIL="watcher@example.test"),
        )

    def repository(self, name="repo"):
        repo = self.root / name
        repo.mkdir(parents=True)
        self.git(repo, "init", "-b", "main")
        (repo / "tracked").write_text("original\n")
        self.git(repo, "add", "tracked")
        self.git(repo, "commit", "-m", "initial")
        return repo

    def test_shared_cold_start_13_and_64_readers(self):
        for count in (13, 64):
            with self.subTest(readers=count):
                directory = self.root / str(count)
                directory.mkdir()
                barrier = MP.Barrier(count)
                processes = [MP.Process(target=compete, args=(directory, barrier)) for _ in range(count)]
                try:
                    for process in processes:
                        process.start()
                    for process in processes:
                        process.join(timeout=35)
                        self.assertEqual(process.exitcode, 0)
                finally:
                    for process in processes:
                        if process.is_alive():
                            process.kill()
                            process.join()
                self.assertEqual((directory / "collections").read_text(), "scan\n")
                self.assertEqual(module.Cache(directory, 60).read(["discovery", "root", 4])["data"], ["repository"])

    def test_overlapping_roots_share_repository_status(self):
        repo = self.repository("parent/repo")
        watchers = [module.Watcher(root, 4, 60, self.root / "shared", cwd=root)
                    for root in (self.root, repo.parent, repo, repo / "..")]
        with mock.patch.object(module, "collect_repository", wraps=module.collect_repository) as collect:
            for watcher in watchers:
                watcher.refresh()
            self.assertEqual(collect.call_count, 1)
        self.assertEqual(watchers[1].discovery_key(), watchers[3].discovery_key())

    def test_expiry_failure_throttling_and_atomic_last_good_snapshot(self):
        key = ["discovery", "root", 4]
        with mock.patch.object(module.time, "monotonic", return_value=100):
            self.cache.refresh(key, lambda: ["old"])
        with mock.patch.object(module.time, "monotonic", return_value=101):
            result = self.cache.refresh(key, lambda: self.fail("fresh cache collected twice"))
            self.assertEqual(result["data"], ["old"])
        with mock.patch.object(module.time, "monotonic", return_value=161):
            def fail():
                raise subprocess.TimeoutExpired("git", 10)
            result = self.cache.refresh(key, fail)
            self.assertEqual(result["data"], ["old"])
            self.assertEqual(result["updated"], 100)
            self.assertEqual(result["error"], "Git timed out")
            self.assertIn("stale", module.health(result, 1))
            self.cache.refresh(key, lambda: self.fail("error retried immediately"))
        with mock.patch.object(module.time, "monotonic", return_value=222):
            result = self.cache.refresh(key, lambda: ["new"])
            self.assertEqual(result["data"], ["new"])
            self.assertEqual(result["error"], "")
        self.assertEqual(len(list(self.cache.directory.iterdir())), 2)

    def test_faster_readers_refresh_slow_reader_cache(self):
        key = ["discovery", "root", 4]
        with mock.patch.object(module.time, "monotonic", return_value=100):
            self.cache.refresh(key, lambda: ["old"])
        with mock.patch.object(module.time, "monotonic", return_value=102):
            fast = module.Cache(self.cache.directory, 1)
            fast.refresh(key, lambda: ["new"])
            self.assertEqual(self.cache.read(key)["data"], ["new"])

    def test_refresh_deadline_tracks_cache_expiry(self):
        key = ["discovery", "root", 4]
        with mock.patch.object(module.time, "monotonic", return_value=100):
            value = self.cache.refresh(key, lambda: ["repo"])
        with mock.patch.object(module.time, "monotonic", return_value=159.5):
            self.assertEqual(self.cache.refresh_in(value), 0.5)
        with mock.patch.object(module.time, "monotonic", return_value=160):
            self.assertEqual(self.cache.refresh_in(value), 0)

        snapshot = {"data": ["repo"], "updated": 100, "error": ""}
        with mock.patch.object(module.time, "monotonic", return_value=102.1):
            self.assertEqual(module.health(snapshot, 1), "")
        with mock.patch.object(module.time, "monotonic", return_value=103.1):
            self.assertEqual(module.health(snapshot, 1), "stale 3s")

    def test_collector_death_releases_lock_without_removing_it(self):
        entered = MP.Event()
        process = MP.Process(target=hold_collection, args=(self.cache.directory, entered))
        process.start()
        try:
            self.assertTrue(entered.wait(timeout=5))
            key = ["discovery", "root", 4]
            lock = self.cache.path(key).with_suffix(".lock")
            inode = lock.stat().st_ino
            self.assertIsNone(self.cache.refresh(key, lambda: self.fail("lock bypassed")))
            process.kill()
            process.join(timeout=5)
            self.assertEqual(self.cache.refresh(key, lambda: ["recovered"])["data"], ["recovered"])
            self.assertEqual(lock.stat().st_ino, inode)
        finally:
            if process.is_alive():
                process.kill()
                process.join()

    def test_corrupt_or_incompatible_cache_is_rebuilt(self):
        key = ["discovery", "root", 4]
        for content in ("partial {", "[]", '{"version": 0}', '{"data": 1}'):
            self.cache.path(key).with_suffix(".json").write_text(content)
            self.assertIsNone(self.cache.read(key))
            self.assertEqual(self.cache.refresh(key, lambda: ["repo"])["data"], ["repo"])
        value = self.cache.read(key)
        for field, invalid in (("data", 5), ("attempted", "now"), ("error", None),
                               ("boot", "old-boot"), ("key", ["other"]), ("updated", None)):
            modified = dict(value, **{field: invalid})
            self.cache.path(key).with_suffix(".json").write_text(json.dumps(modified))
            self.assertIsNone(self.cache.read(key))

    def test_status_and_existing_lock_never_change_index(self):
        repo = self.repository()
        index = repo / ".git/index"
        before = index.read_bytes(), index.stat().st_mtime_ns, index.stat().st_ino
        tracked = repo / "tracked"
        stat = tracked.stat()
        os.utime(tracked, ns=(stat.st_atime_ns, stat.st_mtime_ns + 2_000_000_000))
        for locked in (False, True):
            if locked:
                (repo / ".git/index.lock").write_text("owned by test writer")
            with mock.patch.dict(os.environ, {"GIT_OPTIONAL_LOCKS": "1"}):
                data = module.collect_repository(repo)
            self.assertEqual(data["paths"], [])
            self.assertEqual((index.read_bytes(), index.stat().st_mtime_ns, index.stat().st_ino), before)
        self.assertEqual((repo / ".git/index.lock").read_text(), "owned by test writer")

    @unittest.skipUnless(shutil.which("strace"), "strace is required for index-lock syscall evidence")
    def test_syscalls_never_attempt_index_lock(self):
        repo = self.repository()
        os.utime(repo / "tracked", (time.time() + 2, time.time() + 2))
        trace = self.root / "trace"
        code = "import runpy,sys; m=runpy.run_path(sys.argv[1]); m['collect_repository'](sys.argv[2])"
        subprocess.run(["strace", "-f", "-e", "trace=file", "-o", str(trace),
                        "python3", "-c", code, str(SCRIPT), str(repo)], check=True, capture_output=True)
        self.assertNotIn("index.lock", trace.read_text())

    def test_one_writer_commits_while_13_readers_scan(self):
        repo = self.repository()
        barrier, finished = MP.Barrier(14), MP.Event()
        processes = [MP.Process(target=read_repeatedly,
                                args=(repo, self.root / "shared", barrier, finished)) for _ in range(13)]
        try:
            for process in processes:
                process.start()
            barrier.wait(timeout=30)
            for number in range(10):
                (repo / "tracked").write_text(f"change {number}\n")
                self.git(repo, "add", "tracked")
                self.git(repo, "commit", "-m", f"change {number}")
            self.assertEqual(self.git(repo, "rev-list", "--count", "HEAD").strip(), b"11")
        finally:
            finished.set()
            for process in processes:
                process.join(timeout=15)
                if process.is_alive():
                    process.kill()
                    process.join()
                self.assertEqual(process.exitcode, 0)
        self.assertEqual(module.collect_repository(repo)["paths"], [])

    def test_git_helper_bounds_time_and_disables_optional_locks(self):
        with mock.patch.object(module.subprocess, "run", return_value=mock.Mock(stdout=b"ok")) as run:
            with mock.patch.dict(os.environ, {"GIT_OPTIONAL_LOCKS": "1"}):
                self.assertEqual(module.git(self.root, "status"), b"ok")
            self.assertEqual(run.call_args.kwargs["env"]["GIT_OPTIONAL_LOCKS"], "0")
            self.assertEqual(run.call_args.kwargs["timeout"], 10)
            self.assertIn("--no-optional-locks", run.call_args.args[0])

    def test_linked_worktrees_and_containing_repo(self):
        repo = self.repository()
        linked = self.root / "linked"
        self.git(repo, "worktree", "add", "-b", "linked", str(linked))
        (linked / "tracked").write_text("changed\n")
        child = repo / "child"
        child.mkdir()
        watcher = module.Watcher(child, 0, 60, self.root / "shared", cwd=child)
        watcher.refresh()
        self.assertEqual(watcher.read()[1][0][0], str(repo))
        watcher = module.Watcher(self.root, 4, 60, self.root / "shared", cwd=self.root)
        watcher.refresh()
        data = dict(watcher.read()[1])
        self.assertEqual(data[str(repo)]["data"]["unstaged"], 0)
        self.assertEqual(data[str(linked)]["data"]["unstaged"], 1)
        self.assertNotEqual(watcher.repository_key(repo), watcher.repository_key(linked))

    def test_unusual_paths_rename_and_mixed_changes(self):
        repo = self.repository()
        unusual = "renamed\n\t\033file"
        self.git(repo, "mv", "tracked", unusual)
        (repo / unusual).write_text("changed after staging\n")
        (repo / "new file").write_text("untracked\n")
        data = module.collect_repository(repo)
        self.assertEqual((data["staged"], data["unstaged"], data["untracked"]), (1, 2, 1))
        self.assertTrue(any(" -> " in path for path in data["paths"]))
        self.assertTrue(any("\\n" in path for path in data["paths"]))
        self.assertFalse(any("\033" in path for path in data["paths"]))
        self.assertTrue(module.valid_repository(data))
        self.assertFalse(module.valid_repository({}))

    def test_upstream_detached_and_unborn_branches(self):
        repo = self.repository()
        self.git(repo, "update-ref", "refs/remotes/origin/main", "HEAD")
        self.git(repo, "config", "remote.origin.url", str(repo))
        self.git(repo, "config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*")
        self.git(repo, "branch", "--set-upstream-to=origin/main")
        self.assertEqual(module.collect_repository(repo)["sync"], "synced")
        (repo / "tracked").write_text("ahead\n")
        self.git(repo, "commit", "-am", "ahead")
        self.assertEqual(module.collect_repository(repo)["sync"], "ahead +1")
        self.git(repo, "update-ref", "-d", "refs/remotes/origin/main")
        self.assertEqual(module.collect_repository(repo)["sync"], "upstream-missing")
        self.git(repo, "checkout", "--detach")
        self.assertEqual(module.collect_repository(repo)["branch"], self.git(repo, "rev-parse", "HEAD").decode()[:7])
        self.git(repo, "checkout", "--orphan", "unborn")
        self.assertEqual(module.collect_repository(repo)["branch"], "unborn")

    def test_deleted_repository_and_display_sizes(self):
        repo = self.repository()
        watcher = module.Watcher(repo, 4, 60, self.root / "shared", cwd=repo)
        self.assertIn("loading", module.render(watcher, 80, 24))
        watcher.refresh()
        self.assertIn("branch: main", module.render(watcher, 80, 24))
        self.assertEqual(len(module.render(watcher, 20, 1).splitlines()), 1)
        self.assertIn("more lines", module.render(watcher, 20, 2))
        shutil.rmtree(repo)
        watcher.cache.interval = 0
        watcher.refresh()
        frame = module.render(watcher, 80, 24)
        self.assertIn("stale", frame)
        self.assertIn("scan failed", frame)
        self.assertNotIn("all clean and synced", frame)

    def test_settings_validation(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(module.settings()[1:], (4, 1))
        for interval, depth in (("0", "4"), ("nan", "4"), ("inf", "4"), ("1", "-1"), ("bad", "4")):
            with mock.patch.dict(os.environ, {"WORKLANE_GIT_DIFF_INTERVAL": interval, "WORKLANE_GIT_DIFF_MAX_DEPTH": depth}):
                with self.assertRaises(ValueError):
                    module.settings()

    def test_discovery_depth_missing_root_and_environment_isolation(self):
        repo = self.repository("outer/nested")
        self.assertEqual(module.discover(str(self.root), 2), [])
        self.assertEqual(module.discover(str(self.root), 3), [str(repo)])
        with self.assertRaises(FileNotFoundError):
            module.discover(self.root / "missing", 4)
        self.assertIsNone(module.containing_repository(self.root))
        first = module.Watcher(repo, 4, 1, self.root / "shared")
        with mock.patch.dict(os.environ, {"GIT_INDEX_FILE": str(self.root / "another-index")}):
            second = module.Watcher(repo, 4, 1, self.root / "shared")
        self.assertNotEqual(first.repository_key(repo), second.repository_key(repo))

    def test_porcelain_conflicts_ignored_and_divergence(self):
        status = (b"# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -3\0"
                  b"! ignored\0u UU N... 100644 100644 100644 100644 a b c conflict\0")
        with mock.patch.object(module, "git", return_value=status):
            data = module.collect_repository(self.root)
        self.assertEqual(data["sync"], "diverged +2 -3")
        self.assertEqual(data["paths"], ["UU conflict"])
        self.assertEqual((data["staged"], data["unstaged"]), (1, 1))
        with mock.patch.object(module, "git", return_value=status.replace(b"+2 -3", b"+0 -3")):
            self.assertEqual(module.collect_repository(self.root)["sync"], "behind -3")

    def test_render_clean_and_many_paths_and_unavailable(self):
        repo = self.repository()
        watcher = module.Watcher(repo, 4, 60, self.root / "shared", cwd=repo)
        watcher.refresh()
        data = module.collect_repository(repo)
        data["sync"] = "synced"
        watcher.cache.interval = 0
        watcher.cache.refresh(watcher.repository_key(repo), lambda: data)
        with mock.patch.dict(os.environ, {"WORKLANE_NAME": "test-lane"}):
            frame = module.render(watcher, 80, 24)
        self.assertIn("lane: test-lane", frame)
        self.assertIn("all clean and synced", frame)
        data["paths"] = ["?? file" + str(n) for n in range(20)]
        watcher.cache.refresh(watcher.repository_key(repo), lambda: data)
        self.assertIn("8 more", module.render(watcher, 80, 40))
        self.assertEqual(module.health({"data": None, "error": "failed"}, 1), "unavailable: failed")

    def test_main_render_loop_restores_terminal_and_reports_errors(self):
        repo = self.repository()
        watcher = module.Watcher(repo, 4, 60, self.root / "shared", cwd=repo)
        handlers = {}

        class ImmediatePool:
            def __init__(self, **kwargs):
                pass

            def submit(self, function):
                function()
                return mock.Mock(done=lambda: True, result=lambda: None)

            def shutdown(self, **kwargs):
                pass

        sleeps = 0

        def sleep(_delay):
            nonlocal sleeps
            sleeps += 1
            if sleeps == 2:
                handlers[signal.SIGTERM](signal.SIGTERM, None)

        with mock.patch.object(module, "Watcher", return_value=watcher), \
                mock.patch.object(module.signal, "signal", side_effect=lambda s, fn: handlers.update({s: fn})), \
                mock.patch.object(module.concurrent.futures, "ThreadPoolExecutor", ImmediatePool), \
                mock.patch.object(module.time, "sleep", side_effect=sleep), \
                mock.patch("builtins.print") as output:
            self.assertEqual(module.main(), 0)
            redraws = [call.args[0] for call in output.call_args_list if "\033[?2026h" in call.args[0]]
            self.assertTrue(redraws)
            self.assertTrue(all(frame.endswith("\033[?2026l") for frame in redraws))
            self.assertTrue(all("\033[2J" not in frame and "\033[3J" not in frame for frame in redraws))
            self.assertTrue(all("\033[K\n" in frame for frame in redraws))
            self.assertIn("\033[?1049l", output.call_args_list[-1].args[0])
            with mock.patch.object(module, "render", side_effect=OSError("display failed")):
                self.assertEqual(module.main(), 1)
        with mock.patch.object(module, "settings", side_effect=ValueError("invalid interval")), \
                mock.patch("builtins.print") as output:
            self.assertEqual(module.main(), 1)
            self.assertEqual(output.call_args.args[0], "invalid interval")

    def test_real_pty_watcher_renders_and_exits_cleanly(self):
        repo = self.repository()
        master, slave = pty.openpty()
        process = subprocess.Popen(
            ["python3", str(SCRIPT)], cwd=repo, stdin=slave, stdout=slave, stderr=slave,
            env=dict(os.environ, HOME=str(self.root), WORKLANE_GIT_DIFF_ROOT=str(repo),
                     WORKLANE_GIT_DIFF_INTERVAL="0.05"),
        )
        os.close(slave)
        output = b""
        try:
            deadline = time.monotonic() + 10
            while b"branch: main" not in output and time.monotonic() < deadline:
                if select.select([master], [], [], 0.1)[0]:
                    output += os.read(master, 65536)
            self.assertIn(b"branch: main", output)
            healthy_deadline = time.monotonic() + 0.5
            while time.monotonic() < healthy_deadline:
                if select.select([master], [], [], 0.05)[0]:
                    output += os.read(master, 65536)
            self.assertNotIn(b"stale", output)
            self.assertIn(b"\x1b[?2026h", output)
            self.assertIn(b"\x1b[?2026l", output)
            process.terminate()
            process.wait(timeout=12)
            while select.select([master], [], [], 0.1)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
            self.assertEqual(process.returncode, 0)
            self.assertIn(b"\x1b[?1049l", output)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)


if __name__ == "__main__":
    unittest.main()
