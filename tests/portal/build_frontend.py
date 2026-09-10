"""Build a source snapshot, leaving the submodule and its builds untouched."""

from pathlib import Path
import os
import shutil
import subprocess
import sys


destination = Path(sys.argv[1]).resolve()
source = Path(__file__).resolve().parents[2] / "xdg-desktop-portal"
destination.mkdir(parents=True, exist_ok=True)
# Match the libdex revision in the patched frontend's build.containerfile.
subprocess.run(
    [
        "git",
        "clone",
        "--no-checkout",
        "https://gitlab.gnome.org/GNOME/libdex.git",
        str(destination / "libdex"),
    ],
    check=True,
)
subprocess.run(
    ["git", "checkout", "--detach", "c81ea8719a4b257f2fd3fec34b6e16859400eed8"],
    cwd=destination / "libdex",
    check=True,
)
prefix = destination / "deps"
subprocess.run(
    [
        "meson",
        "setup",
        str(destination / "libdex-build"),
        str(destination / "libdex"),
        f"--prefix={prefix}",
        "--libdir=lib",
        "-Dgdbus=enabled",
        "-Dintrospection=disabled",
        "-Dvapi=false",
        "-Dexamples=false",
        "-Dtests=false",
    ],
    check=True,
)
subprocess.run(
    ["meson", "install", "-C", str(destination / "libdex-build")], check=True
)
env = os.environ.copy()
env["PKG_CONFIG_PATH"] = (
    str(prefix / "lib/pkgconfig") + ":" + env.get("PKG_CONFIG_PATH", "")
)
env["LD_LIBRARY_PATH"] = str(prefix / "lib") + ":" + env.get("LD_LIBRARY_PATH", "")
shutil.copytree(
    source,
    destination / "source",
    ignore=shutil.ignore_patterns(
        ".git", "build*", "_build*", "__pycache__", ".pytest_cache", ".wraplock"
    ),
)
subprocess.run(
    [
        "meson",
        "setup",
        str(destination / "build"),
        str(destination / "source"),
        "-Dtests=enabled",
        "-Ddocumentation=disabled",
        "-Dman-pages=disabled",
        "-Dflatpak-interfaces=disabled",
        "-Dgeoclue=disabled",
        "-Dgudev=disabled",
        "-Dsystemd=disabled",
    ],
    check=True,
    env=env,
)
subprocess.run(
    ["meson", "compile", "-C", str(destination / "build")], check=True, env=env
)
