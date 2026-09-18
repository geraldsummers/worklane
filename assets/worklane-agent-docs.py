"""Install the embedded reference bundle during lane attach; never migrate user rules."""

import fcntl
import hashlib
import json
import os
from pathlib import Path
import tempfile


def install(home: Path, documents: dict[str, str]) -> str:
    """Publish a complete content-addressed bundle and seed absent global instructions."""
    revision = hashlib.sha256(
        json.dumps(documents, sort_keys=True, ensure_ascii=True).encode()
    ).hexdigest()
    files = {**documents, "REVISION": revision + "\n"}
    root = home / ".local/share/worklane/agent-docs"
    versions = root / "versions"
    versions.mkdir(parents=True, exist_ok=True)
    # Keep the lock inode stable across attaches, including concurrent controllers.
    with (root / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        destination = versions / revision
        with tempfile.TemporaryDirectory(prefix=".stage-", dir=root) as temporary:
            stage = Path(temporary)
            bundle = stage / "bundle"
            bundle.mkdir()
            for name, content in files.items():
                with (bundle / name).open("w", encoding="utf-8") as stream:
                    stream.write(content)
                    stream.flush()
                    os.fsync(stream.fileno())
            if destination.exists():
                if destination.is_symlink() or any(
                    (destination / name).is_symlink()
                    or (destination / name).read_text(encoding="utf-8") != content
                    for name, content in files.items()
                ):
                    raise ValueError(f"managed agent guidance was modified: {destination}")
            else:
                os.rename(bundle, destination)
            current = root / "current"
            target = Path("versions") / revision
            if not current.is_symlink() or current.readlink() != target:
                pointer = stage / "current"
                pointer.symlink_to(target)
                os.replace(pointer, current)
            codex = home / ".codex"
            codex.mkdir(exist_ok=True)
            active = codex / "AGENTS.md"
            override = codex / "AGENTS.override.md"
            # lexists also protects dangling symlinks and empty files.
            if not os.path.lexists(active) and not os.path.lexists(override):
                seed = stage / "seed"
                seed.write_text(
                    documents["lane.md"]
                    + f"\n<!-- Worklane guidance bundle: {revision} -->\n",
                    encoding="utf-8",
                )
                try:
                    # Link a complete file without replacing a concurrent creator's file.
                    os.link(seed, active)
                except FileExistsError:
                    pass
    return revision
