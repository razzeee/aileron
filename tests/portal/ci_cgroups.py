"""Run a CI command with nested controllers in a disposable Docker container.

Never run on the host or with a host cgroup namespace/mount.
"""

from pathlib import Path
import subprocess
import sys


def main():
    root = Path("/sys/fs/cgroup")
    # Require Docker's private cgroup namespace and its namespace-root mount.
    # A host namespace exposes PID 1 in /init.scope (or another host path).
    if (
        not Path("/.dockerenv").exists()
        or Path("/proc/1/cgroup").read_text() != "0::/\n"
    ):
        raise RuntimeError(
            "requires a disposable Docker container with --cgroupns=private"
        )
    mount = next(
        line.split()
        for line in Path("/proc/self/mountinfo").read_text().splitlines()
        if line.split()[4] == str(root)
    )
    if mount[3] != "/" or "cgroup2" not in mount or "rw" not in mount[5].split(","):
        raise RuntimeError("requires a writable, namespace-root cgroup v2 mount")
    required = {"memory", "pids"}
    available = set((root / "cgroup.controllers").read_text().split())
    original = set((root / "cgroup.subtree_control").read_text().split())
    print(
        f"CI cgroup controllers: available={sorted(available)}, enabled={sorted(original)}",
        flush=True,
    )
    if not required <= available:
        raise RuntimeError(f"Docker has not delegated {sorted(required - available)}")
    if original or (root / "cgroup.type").read_text().strip() != "domain":
        raise RuntimeError(
            "requires a fresh Docker job cgroup with no child controllers enabled"
        )
    leaf = root / "aileron-ci-processes"
    leaf.mkdir()

    def move(source, destination):
        for pid in (source / "cgroup.procs").read_text().split():
            try:
                (destination / "cgroup.procs").write_text(pid)
            except ProcessLookupError:
                pass

    try:
        # cgroup v2 forbids domain controllers on a populated internal node.
        # Move PID 1 and the exec shell too, not just this Python process.
        move(root, leaf)
        (root / "cgroup.subtree_control").write_text("+memory +pids")
        subprocess.run(
            [sys.executable, str(Path(__file__).with_name("probe_crun.py"))], check=True
        )
        sys.exit(subprocess.call(sys.argv[1:]))
    finally:
        # crun may enable additional controllers. Restore before Actions starts
        # another docker exec, which joins the original container cgroup.
        enabled = set((root / "cgroup.subtree_control").read_text().split())
        added = enabled - original
        if added:
            (root / "cgroup.subtree_control").write_text(
                " ".join(f"-{c}" for c in sorted(added))
            )
        move(leaf, root)
        leaf.rmdir()


if __name__ == "__main__":
    main()
