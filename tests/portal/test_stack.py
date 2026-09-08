from pathlib import Path
import unittest
from unittest.mock import patch

from stack import find_stub


class FindStubTests(unittest.TestCase):
    def walk(self, samefile, iterdir, read_text, pidfd_open=None):
        with (
            patch("stack.os.path.samefile", side_effect=samefile),
            patch.object(Path, "iterdir", side_effect=iterdir),
            patch.object(Path, "read_text", side_effect=read_text),
            patch(
                "stack.os.pidfd_open", side_effect=pidfd_open or (lambda pid: pid + 100)
            ),
            patch("stack.os.close") as close,
        ):
            result = find_stub(1, Path("/stub"))
        self.assertEqual(result, (3, 103))
        self.assertNotIn(unittest.mock.call(103), close.call_args_list)
        return close

    def test_disappearing_or_inaccessible_executable_keeps_children(self):
        for error in (FileNotFoundError, PermissionError, ProcessLookupError):
            with self.subTest(error=error):
                close = self.walk(
                    [error(), True], [iter([Path("/proc/1/task/1")])], ["3"]
                )
                close.assert_called_once_with(101)

    def test_bad_task_does_not_skip_remaining_threads(self):
        for error in (FileNotFoundError, PermissionError, ProcessLookupError):
            with self.subTest(error=error):
                self.walk(
                    [False, True],
                    [iter([Path("/task/1"), Path("/task/2")])],
                    [error(), "3"],
                )

    def test_missing_task_directory_keeps_pending_siblings(self):
        self.walk(
            [False, False, True],
            [iter([Path("/task/1")]), FileNotFoundError()],
            ["3 2"],
        )

    def test_pidfd_open_race_keeps_pending_siblings(self):
        def open_pidfd(pid):
            if pid == 2:
                raise ProcessLookupError()
            return pid + 100

        close = self.walk([False, True], [iter([Path("/task/1")])], ["3 2"], open_pidfd)
        close.assert_called_once_with(101)

    def test_pidfd_is_open_before_identity_check(self):
        with (
            patch("stack.os.pidfd_open", return_value=101) as opened,
            patch("stack.os.path.samefile") as samefile,
        ):

            def match(*args):
                opened.assert_called_once_with(1)
                return True

            samefile.side_effect = match
            self.assertEqual(find_stub(1, Path("/stub")), (1, 101))

    def test_no_match_fails_and_closes_pidfd(self):
        with (
            patch("stack.os.pidfd_open", return_value=101),
            patch("stack.os.path.samefile", return_value=False),
            patch.object(Path, "iterdir", side_effect=PermissionError()),
            patch("stack.os.close") as close,
        ):
            with self.assertRaisesRegex(AssertionError, "harness-owned OCI stub"):
                find_stub(1, Path("/stub"))
            close.assert_called_once_with(101)

    def test_unexpected_errors_propagate_and_close_pidfd(self):
        with (
            patch("stack.os.pidfd_open", return_value=101),
            patch("stack.os.path.samefile", side_effect=OSError("unexpected")),
            patch("stack.os.close") as close,
        ):
            with self.assertRaisesRegex(OSError, "unexpected"):
                find_stub(1, Path("/stub"))
            close.assert_called_once_with(101)
