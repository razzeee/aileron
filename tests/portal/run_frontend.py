"""Run the submodule's model tests without writing into its source/build tree."""

import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile


def main():
    source = Path(__file__).resolve().parents[2] / "xdg-desktop-portal"
    build = Path(sys.argv[1]).resolve()
    env = os.environ.copy()
    env.update(
        LD_LIBRARY_PATH=str(build.parent / "deps/lib"),
        PYTHONDONTWRITEBYTECODE="1",
        PYTHONPATH=str(source),
        XDG_DESKTOP_PORTAL_PATH=str(build / "desktop-portal/xdg-desktop-portal"),
        XDG_DOCUMENT_PORTAL_PATH=str(build / "document-portal/xdg-document-portal"),
        XDG_PERMISSION_STORE_PATH=str(build / "document-portal/xdg-permission-store"),
        XDP_VALIDATE_ICON=str(
            build / "desktop-portal/xdg-desktop-portal-validate-icon"
        ),
        XDP_VALIDATE_SOUND=str(
            build / "desktop-portal/xdg-desktop-portal-validate-sound"
        ),
    )
    with tempfile.TemporaryDirectory(prefix="aileron-frontend-") as temp:
        process = subprocess.Popen(
            [
                sys.executable,
                "-m",
                "pytest",
                str(source / "tests/test_model_portals.py"),
                "-v",
                "-x",
                "-p",
                "no:cacheprovider",
                "--rootdir",
                temp,
            ],
            cwd=temp,
            env=env,
            start_new_session=True,
        )
        try:
            return process.wait(timeout=240)
        finally:
            # Also reap fixture processes if pytest crashes or exceeds its deadline.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


if __name__ == "__main__":
    sys.exit(main())
