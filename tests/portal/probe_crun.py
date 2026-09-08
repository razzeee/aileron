"""Smoke-test nested crun startup and the stack's actual cgroup limits."""

import json
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


def build_rootfs(rootfs):
    rootfs.mkdir()
    sources = {Path("/bin/sh"), Path("/usr/bin/cat")}
    # ldd is only safe for trusted executables. Never accept payload paths here,
    # or inherit LD_* variables that could change dependency resolution.
    for binary in sorted(sources):
        libraries = subprocess.check_output(
            ["/usr/bin/ldd", str(binary)],
            text=True,
            env={"PATH": "/usr/bin:/bin", "LC_ALL": "C"},
        )
        if "=> not found" in libraries:
            raise RuntimeError(f"Unresolved dependencies for {binary}:\n{libraries}")
        sources.update(
            Path(library)
            for library in re.findall(
                r"(?:=>\s+|^\s*)(/\S+)\s+\(", libraries, re.MULTILINE
            )
        )
    for source in sorted(sources):
        if ".." in source.parts:
            raise RuntimeError(f"Unsafe dependency path: {source}")
        destination = rootfs / source.relative_to("/")
        destination.parent.mkdir(parents=True, exist_ok=True)
        # Dereference both binary and library symlinks, but preserve their load
        # paths. This also handles merged-/usr systems without dangling links.
        shutil.copy2(source, destination)
    # crun also needs /dev when preparing its default devices.
    for directory in ("dev", "proc", "sys/fs/cgroup"):
        (rootfs / directory).mkdir(parents=True)


def main():
    with tempfile.TemporaryDirectory(prefix="aileron-crun-probe-") as temp:
        bundle = Path(temp)
        build_rootfs(bundle / "rootfs")
        config = {
            "ociVersion": "1.0.2",
            "root": {"path": "rootfs", "readonly": True},
            "process": {
                "terminal": False,
                "user": {"uid": 0, "gid": 0},
                "args": [
                    "/bin/sh",
                    "-ec",
                    'test "$(cat /sys/fs/cgroup/memory.max)" = 536870912; test "$(cat /sys/fs/cgroup/pids.max)" = 256',
                ],
                "cwd": "/",
                "env": ["PATH=/usr/bin:/bin"],
            },
            "mounts": [
                {"destination": "/proc", "type": "proc", "source": "proc"},
                {
                    "destination": "/sys/fs/cgroup",
                    "type": "cgroup",
                    "source": "cgroup",
                    "options": ["ro"],
                },
            ],
            "linux": {
                "namespaces": [
                    {"type": name}
                    for name in ("mount", "pid", "cgroup", "network", "ipc", "uts")
                ],
                "resources": {"memory": {"limit": 536870912}, "pids": {"limit": 256}},
            },
        }
        (bundle / "config.json").write_text(json.dumps(config))
        subprocess.run(
            [
                "crun",
                "--root",
                str(bundle / "state"),
                "run",
                "--bundle",
                temp,
                "aileron-probe",
            ],
            check=True,
        )
        print("PASS: nested crun enforces memory.max=536870912 and pids.max=256")


if __name__ == "__main__":
    main()
