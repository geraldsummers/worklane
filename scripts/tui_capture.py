#!/usr/bin/env python3
"""Drive lane in a real PTY and emit inspectable terminal-frame artifacts."""
import argparse, codecs, fcntl, html, json, os, pty, select, signal, struct, subprocess, termios, time
from pathlib import Path

CSI = "\x1b["

class Screen:
    def __init__(self, rows=40, cols=140):
        self.rows, self.cols = rows, cols
        self.grid = [[" " for _ in range(cols)] for _ in range(rows)]
        self.primary_grid = None
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.pending = ""
        self.r = self.c = 0
    def feed(self, data):
        text = self.pending + self.decoder.decode(data)
        self.pending = ""
        i = 0
        while i < len(text):
            ch = text[i]
            if ch == "\x1b" and i + 1 == len(text):
                self.pending = text[i:]
                break
            if ch == "\x1b" and text[i + 1] == "[":
                j = i + 2
                while j < len(text) and not ("@" <= text[j] <= "~"):
                    j += 1
                if j < len(text):
                    self.csi(text[i + 2:j], text[j]); i = j + 1; continue
                self.pending = text[i:]
                break
            if ch == "\x1b" and text[i + 1] == "]":
                j = i + 2
                while j < len(text):
                    if text[j] == "\x07":
                        i = j + 1
                        break
                    if text[j] == "\x1b" and j + 1 < len(text) and text[j + 1] == "\\":
                        i = j + 2
                        break
                    j += 1
                else:
                    self.pending = text[i:]
                    break
                continue
            if ch == "\x1b" and text[i + 1] == "\\":
                i += 2
                continue
            if ch == "\r": self.c = 0
            elif ch == "\n": self.r = min(self.rows - 1, self.r + 1)
            elif ch == "\b": self.c = max(0, self.c - 1)
            elif ch >= " ":
                if self.c < self.cols: self.grid[self.r][self.c] = ch
                self.c = min(self.cols - 1, self.c + 1)
            i += 1
    def csi(self, params, final):
        if params == "?1049" and final == "h":
            self.primary_grid = [row[:] for row in self.grid]
            self.grid = [[" " for _ in range(self.cols)] for _ in range(self.rows)]
            self.r = self.c = 0
            return
        if params == "?1049" and final == "l":
            if self.primary_grid is not None:
                self.grid = self.primary_grid
                self.primary_grid = None
            self.r = self.c = 0
            return
        clean = params.lstrip("?")
        nums = [int(x) if x.isdigit() else 0 for x in clean.split(";")] if clean else [0]
        n = nums[0] or 1
        if final in "Hf":
            self.r = max(0, min(self.rows - 1, (nums[0] or 1) - 1))
            self.c = max(0, min(self.cols - 1, (nums[1] if len(nums)>1 and nums[1] else 1) - 1))
        elif final == "A": self.r = max(0, self.r - n)
        elif final == "B": self.r = min(self.rows - 1, self.r + n)
        elif final == "C": self.c = min(self.cols - 1, self.c + n)
        elif final == "D": self.c = max(0, self.c - n)
        elif final == "J":
            if nums[0] == 0:
                self.grid[self.r][self.c:] = [" "] * (self.cols - self.c)
                for row in range(self.r + 1, self.rows): self.grid[row] = [" "] * self.cols
            elif nums[0] == 1:
                for row in range(self.r): self.grid[row] = [" "] * self.cols
                self.grid[self.r][:self.c + 1] = [" "] * (self.c + 1)
            elif nums[0] in (2, 3): self.grid = [[" " for _ in range(self.cols)] for _ in range(self.rows)]
        elif final == "K":
            if nums[0] == 0: self.grid[self.r][self.c:] = [" "] * (self.cols - self.c)
            elif nums[0] == 1: self.grid[self.r][:self.c + 1] = [" "] * (self.c + 1)
            elif nums[0] == 2: self.grid[self.r] = [" "] * self.cols
    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.grid).rstrip() + "\n"

def drain(fd, screen, transcript, duration):
    end = time.time() + duration
    while time.time() < end:
        ready, _, _ = select.select([fd], [], [], min(0.1, end-time.time()))
        if not ready: continue
        try: chunk = os.read(fd, 65536)
        except OSError: return
        if not chunk: return
        transcript.write(chunk); transcript.flush(); screen.feed(chunk)

def snapshot(screen, out, name):
    text = screen.text()
    (out / f"{name}.txt").write_text(text)
    body = html.escape(text)
    (out / f"{name}.html").write_text(f'''<!doctype html><meta charset="utf-8"><style>body{{margin:0;background:#111;color:#eee}}pre{{font:16px/1.25 monospace;padding:18px}}</style><pre>{body}</pre>''')

def main():
    ap=argparse.ArgumentParser(); ap.add_argument("--out",required=True); ap.add_argument("--scenario",required=True); ap.add_argument("command",nargs=argparse.REMAINDER)
    a=ap.parse_args(); out=Path(a.out); out.mkdir(parents=True,exist_ok=True)
    scenario=json.loads(Path(a.scenario).read_text()); master, slave=pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
    env=os.environ.copy(); env.update({"TERM":"xterm-256color","COLUMNS":"140","LINES":"40"})
    proc=subprocess.Popen(a.command,stdin=slave,stdout=slave,stderr=slave,env=env,start_new_session=True); os.close(slave)
    screen=Screen(); transcript=(out/"transcript.ansi").open("wb")
    terminated_by_harness = False
    try:
        drain(master,screen,transcript,scenario.get("startup_wait",2))
        snapshot(screen,out,"00-start")
        for index,step in enumerate(scenario["steps"],1):
            if proc.poll() is not None:
                raise RuntimeError(f"TUI exited early with {proc.returncode}")
            if command := step.get("exec"):
                completed = subprocess.run(command, env=env, capture_output=True, text=True)
                (out / f"{index:02d}-{step['name']}.exec.txt").write_text(
                    completed.stdout + completed.stderr
                )
                if completed.returncode != 0:
                    raise RuntimeError(
                        f"step {step['name']!r} command failed with {completed.returncode}"
                    )
            os.write(master,step.get("keys","").encode())
            if expected := step.get("until"):
                deadline = time.time() + step.get("timeout", 120)
                while expected not in screen.text() and time.time() < deadline:
                    drain(master, screen, transcript, max(0.0, min(0.5, deadline - time.time())))
                if expected not in screen.text():
                    raise RuntimeError(
                        f"step {step['name']!r} did not display {expected!r} before timeout"
                    )
            else:
                drain(master,screen,transcript,step.get("wait",1))
            snapshot(screen,out,f"{index:02d}-{step['name']}")
        os.write(master,b"q"); drain(master,screen,transcript,3)
    finally:
        if proc.poll() is None:
            terminated_by_harness = True
            os.killpg(proc.pid,signal.SIGTERM)
        proc.wait(timeout=5); os.close(master); transcript.close()
    (out/"result.json").write_text(json.dumps({"exit_code":proc.returncode,"snapshots":len(scenario["steps"])+1},indent=2))
    return 0 if proc.returncode in (0,-signal.SIGTERM) or terminated_by_harness else proc.returncode
if __name__=="__main__": raise SystemExit(main())
