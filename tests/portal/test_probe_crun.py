import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import probe_crun


class ProbeCrunTests(unittest.TestCase):
    def test_probe_uses_separate_root_and_default_pivot(self):
        run = subprocess.run

        def inspect(command, **kwargs):
            if command[0] != "crun":
                return run(command, **kwargs)
            self.assertEqual(command[3], "run")
            self.assertNotIn("--no-pivot", command)
            bundle = Path(command[command.index("--bundle") + 1])
            config = json.loads((bundle / "config.json").read_text())
            rootfs = (bundle / config["root"]["path"]).resolve()
            self.assertEqual(rootfs, bundle / "rootfs")
            self.assertTrue(config["root"]["readonly"])
            for path in ("bin/sh", "usr/bin/cat"):
                self.assertTrue((rootfs / path).is_file())
                self.assertFalse((rootfs / path).is_symlink())
            for path in ("dev", "proc", "sys/fs/cgroup"):
                self.assertTrue((rootfs / path).is_dir())
            self.assertEqual(
                {mount["type"] for mount in config["mounts"]}, {"proc", "cgroup"}
            )
            self.assertEqual(
                config["linux"]["resources"],
                {"memory": {"limit": 536870912}, "pids": {"limit": 256}},
            )
            self.assertIn("memory.max", config["process"]["args"][2])
            self.assertIn("pids.max", config["process"]["args"][2])
            return subprocess.CompletedProcess(command, 0)

        with (
            patch("probe_crun.subprocess.run", side_effect=inspect) as mocked,
            patch("builtins.print"),
        ):
            probe_crun.main()
        self.assertEqual(sum(call.args[0][0] == "crun" for call in mocked.call_args_list), 1)

    @unittest.skipUnless(os.geteuid() == 0, "requires root for isolated chroot execution")
    def test_shell_and_cat_execute_with_only_copied_dependencies(self):
        with tempfile.TemporaryDirectory() as temp:
            rootfs = Path(temp) / "rootfs"
            probe_crun.build_rootfs(rootfs)
            (rootfs / "message").write_text("isolated rootfs\n")
            result = subprocess.check_output(
                ["chroot", str(rootfs), "/bin/sh", "-ec", "cat /message"],
                text=True,
                env={"PATH": "/usr/sbin:/usr/bin:/bin", "LC_ALL": "C"},
            )
            self.assertEqual(result, "isolated rootfs\n")

    def test_missing_library_fails_before_crun(self):
        with tempfile.TemporaryDirectory() as temp:
            with patch(
                "probe_crun.subprocess.check_output",
                return_value="libmissing.so => not found\n",
            ):
                with self.assertRaisesRegex(RuntimeError, "Unresolved dependencies"):
                    probe_crun.build_rootfs(Path(temp) / "rootfs")
