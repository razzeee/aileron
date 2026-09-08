"""Smoke-test nested crun startup and the stack's actual cgroup limits."""

import json
from pathlib import Path
import subprocess
import tempfile


def main():
    with tempfile.TemporaryDirectory(prefix="aileron-crun-probe-") as temp:
        bundle = Path(temp)
        config = {
            "ociVersion": "1.0.2",
            "root": {"path": "/", "readonly": True},
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
