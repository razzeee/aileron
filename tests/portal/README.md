# Portal integration tests

Run from the repository root with system Python (dbus, dbusmock, PyGObject,
pytest and UMockdev installed). The CI dependency list is in
`.github/workflows/ci.yml`. No display or downloaded model is required.

```sh
python3 tests/portal/build_frontend.py /tmp/aileron-test-build
cargo build --locked -p aileron-daemon -p aileron-portal -p aileron-runtime
python3 tests/portal/run_frontend.py /tmp/aileron-test-build/build
python3 tests/portal/stack.py /tmp/aileron-test-build/build --check-cancellation
```

Use a new build destination. The builder copies the checked-out frontend source
without editing the submodule or any existing build. It builds the libdex commit
used by upstream CI into a local prefix. Distribution libdex 1.1.0 is insufficient:
it lacks `dex_scheduler_spawnv`, despite satisfying the frontend's version check.
GLib 2.88 or newer is also required. Nothing is installed into system directories.

## Coverage

`run_frontend.py` executes the existing `test_model_portals.py` suite, unchanged,
with private session/system buses and temporary data directories supplied by its
fixtures. It covers Language, SpokenLanguage and Vision using mock backends,
including FD validation, signal routing/privacy, request cancellation, session
ownership, client disconnect and shutdown. These are not real Rust-backend tests.
Other upstream portal suites are not run by this command.

`stack.py` uses the real frontend, Rust backend, daemon, and compiled Rust stub
inside the daemon's real crun container wrapper. Its CI checks cover:

- Persisted permission denial returned through public D-Bus, with no daemon session.
- Permission grant through the management API followed by public session creation.
- Language `StreamResponse` tokens, request/session correlation and terminal success.
- Guided snapshots, terminal tool calls, and `StreamSubmitToolResultsGuided`
  continuation through the real stub runtime.
- Public `Request.Close` after work reaches the stub, terminating the stopped payload
  process, without a response or tokens for the cancelled request; the session remains.
- Explicit public session close removing the daemon session.
- Disconnect of an idle public client removing its daemon session.
- Permission revocation persisted through the management API, removing two existing
  matching daemon sessions and denying new public sessions.
- Regrant allowing a new session without reviving revoked public session handles;
  streaming through either old handle is rejected without tokens.

The host app identity comes from the frontend's existing test override. This does
not test real Flatpak identity discovery or a graphical permission prompt. The
rootfs contains the compiled stub and its ELF libraries, not the shipped Docker
image. Speech, vision, GPU runtimes, in-flight disconnect, revocation during active
inference, failed permission persistence, model installation and post-cancellation
streaming recovery are not covered by the real-stack check.

The harness uses a private bus with no service activation directories, temporary
HOME/XDG/Aileron paths, a memory GSettings backend, and no inherited Aileron
permission bypasses. It starts and stops its own services only. Run as an ordinary
user with working rootless crun, or inside a disposable privileged CI container.
The CI container needs nested namespaces for crun; no host services are installed
or restarted. Stack process logs are printed on failure.

CI uses `--privileged --cgroupns=private` and wraps the real-stack command with
`ci_cgroups.py`. Docker initially puts PID 1 and exec processes at the cgroup
namespace root. cgroup v2 cannot enable the memory controller for children while
that root is populated, so privilege alone does not make nested limits work.
The wrapper moves the job processes into a leaf, enables memory and PID
controllers at the now-empty root, and runs `probe_crun.py` to verify the actual
512 MiB memory and 256 PID limits inside crun. It then runs the stack with all
assertions enabled. In `finally`, it restores the original layout so subsequent
Actions Docker execs can join the job cgroup again.

These cgroup tools are for a fresh, disposable Docker container only, never the
host, a host cgroup namespace, or a bind mount of host cgroups. No host cgroup
configuration or services need changing. To check delegation without building
the frontend, run `python3 tests/portal/ci_cgroups.py true` inside that container
with Python, crun, `/bin/sh`, `/usr/bin/cat` and `/usr/bin/ldd` installed. The
wrapper always runs the limit probe first. The probe copies these two trusted
system binaries and their ELF libraries into a temporary, read-only rootfs, with
fresh proc and cgroup mounts. It uses crun's default `pivot_root`, not the outer
Docker root or a pivot bypass.

Process-walk and probe regression tests use Python's standard library. Probe
tests also need the system binaries above; the isolated shell/`cat` execution
test needs root for `chroot` and skips otherwise. Run that test in the disposable
container, not by elevating the host test command:

```sh
python3 -B -m unittest discover -s tests/portal -p 'test_*.py'
```

## Deterministic cancellation

```sh
python3 tests/portal/stack.py /tmp/aileron-test-build/build --check-cancellation
```

CI always passes `--check-cancellation`. The check pauses only the stub found under
the harness daemon, checks its executable inode, and uses a pidfd to signal it
safely. It waits for queued bytes in the stub's input pipe before closing the public
request, then requires the payload pidfd to report process exit. This avoids racing
the stub's immediate token output and verifies that cancellation terminates the
payload, not just the crun supervisor. Cleanup resumes the stub even on failure.
Omitting the flag runs the other lifecycle checks and reports cancellation as not run.
