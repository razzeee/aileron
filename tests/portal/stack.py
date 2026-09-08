"""Public D-Bus -> patched frontend -> Rust backend -> daemon -> OCI stub.

No installed services, session bus, model downloads or user data are used.
Run after cargo build -p aileron-daemon -p aileron-portal -p aileron-runtime.
"""

import argparse
import array
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import select
import signal
import socket
import subprocess
import sys
import tempfile
import termios
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("build_dir", type=Path)
    parser.add_argument("--bin-dir", type=Path, default=Path("target/debug"))
    parser.add_argument(
        "--check-cancellation",
        action="store_true",
        help="also verify in-flight cancellation and stub termination (enabled in CI)",
    )
    args = parser.parse_args()
    build = args.build_dir.resolve()
    binaries = args.bin_dir.resolve()
    source = Path(__file__).resolve().parents[2] / "xdg-desktop-portal"
    sys.dont_write_bytecode = True
    sys.path.insert(0, str(source))
    import dbus
    import tests.xdp_utils as xdp

    app_id = "org.aileron.StackTest"
    use_case = "language.summarize"
    backend_name = "org.freedesktop.impl.portal.desktop.aileron"
    with (
        tempfile.TemporaryDirectory(prefix="aileron-stack-") as temp,
        ExitStack() as stack,
    ):
        root = Path(temp)
        # Do not inherit developer overrides, prompt helpers or activation paths.
        env = {"PATH": os.environ["PATH"], "LANG": "C.UTF-8"}
        for key in (
            "HOME",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
            "XDG_DATA_DIRS",
            "XDG_CONFIG_DIRS",
            "AILERON_SYSTEM_DATA_DIR",
            "AILERON_MANIFEST_DIRS",
            "XDG_DESKTOP_PORTAL_DIR",
        ):
            path = root / key.lower()
            path.mkdir(mode=0o700)
            env[key] = str(path)
        env.update(
            LD_LIBRARY_PATH=str(build.parent / "deps/lib"),
            GSETTINGS_SCHEMA_DIR="/usr/share/glib-2.0/schemas",
            GSETTINGS_BACKEND="memory",
            AILERON_DATA_HOME=env["XDG_DATA_HOME"],
            AILERON_RUNTIME_DIR=env["XDG_RUNTIME_DIR"],
            AILERON_VARIANT="cpu",
            AILERON_OCI_STORE=str(root / "oci"),
            AILERON_CONTAINER_MEMORY="512m",
            XDG_CURRENT_DESKTOP="aileron-test",
            XDG_DESKTOP_PORTAL_TEST_APP_INFO_KIND="host",
            XDG_DESKTOP_PORTAL_TEST_HOST_APPID=app_id,
        )
        processes = []

        def start(name, command):
            log = stack.enter_context((root / f"{name}.log").open("w+"))
            process = subprocess.Popen(
                command, env=env, cwd=root, stdout=log, stderr=log
            )
            processes.append((name, process, log))

            def stop():
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)

            stack.callback(stop)
            return process

        def wait_for(predicate):
            deadline = time.monotonic() + 15
            while not predicate():
                for name, process, _ in processes:
                    assert (
                        process.poll() is None
                    ), f"{name} exited: {process.returncode}"
                assert time.monotonic() < deadline, "stack readiness/cleanup timed out"
                time.sleep(0.02)

        def management(method, **parameters):
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(5)
                connection.connect(
                    str(Path(env["AILERON_RUNTIME_DIR"]) / "aileron.socket")
                )
                connection.sendall(
                    json.dumps({"method": method, "parameters": parameters}).encode()
                    + b"\0"
                )
                response = b""
                while b"\0" not in response:
                    chunk = connection.recv(65536)
                    assert chunk, "daemon closed management connection without replying"
                    response += chunk
                result = json.loads(response.split(b"\0", 1)[0])
                assert "error" not in result, result
                return result.get("parameters", {})

        # Use the actual compiled stub with its ELF libraries, not a protocol mock.
        rootfs = root / "oci/rootfs/aileron_stub_ci"
        rootfs.mkdir(parents=True)
        stub = binaries / "aileron-runtime-stub"
        shutil.copy2(stub, rootfs / "entrypoint")
        libraries = subprocess.check_output(["ldd", str(stub)], text=True)
        assert "not found" not in libraries, libraries
        for library in set(re.findall(r"/[^\s()]+", libraries)):
            destination = rootfs / library.lstrip("/")
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(library, destination)
        for directory in ("proc", "tmp", "dev/shm", "model"):
            (rootfs / directory).mkdir(parents=True, exist_ok=True)

        data = Path(env["AILERON_DATA_HOME"]) / "aileron"
        (data / "profiles").mkdir(parents=True)
        artifacts = root / "artifacts"
        artifacts.mkdir()
        profile = dict(
            profile_id="ci-stub",
            model_id="ci-stub",
            runtime_id="llm-vision-whisper",
            artifact_path=str(artifacts),
            runtime_images=[dict(variant="cpu", image_ref="aileron/stub:ci")],
            use_cases=[use_case],
            installed_at="2026-01-01T00:00:00Z",
        )
        (data / "profiles/ci-stub.json").write_text(json.dumps(profile))
        (data / "assignments.json").write_text(json.dumps({use_case: "ci-stub"}))
        (data / "permissions.json").write_text(
            json.dumps(
                {
                    f"{app_id}/{use_case}": {"allowed": False, "last_used": None},
                }
            )
        )
        portals = Path(env["XDG_DESKTOP_PORTAL_DIR"])
        (portals / "aileron-test-portals.conf").write_text(
            "[preferred]\ndefault=aileron;\n"
        )
        (portals / "aileron.portal").write_text(
            f"[portal]\nDBusName={backend_name}\nInterfaces="
            "org.freedesktop.impl.portal.Language;org.freedesktop.impl.portal.SpokenLanguage;"
            "org.freedesktop.impl.portal.Vision;\n"
        )
        # No <standard_session_servicedirs/>: even accidental activation stays isolated.
        bus_path = root / "bus"
        config = root / "bus.conf"
        config.write_text(
            f"<busconfig><type>session</type><listen>unix:path={bus_path}</listen>"
            '<policy context="default"><allow own="*"/><allow send_destination="*"/>'
            '<allow receive_sender="*"/></policy></busconfig>'
        )
        env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path={bus_path}"
        env["DBUS_SYSTEM_BUS_ADDRESS"] = env["DBUS_SESSION_BUS_ADDRESS"]

        try:
            start("bus", ["dbus-daemon", "--nofork", f"--config-file={config}"])
            wait_for(bus_path.exists)
            bus = dbus.bus.BusConnection(env["DBUS_SESSION_BUS_ADDRESS"])
            stack.callback(bus.close)
            daemon = start("daemon", [str(binaries / "aileron-daemon")])
            wait_for((Path(env["AILERON_RUNTIME_DIR"]) / "aileron.socket").exists)
            start("backend", [str(binaries / "aileron-portal")])
            wait_for(lambda: bus.name_has_owner(backend_name))
            start("permissions", [str(build / "document-portal/xdg-permission-store")])
            wait_for(
                lambda: bus.name_has_owner(
                    "org.freedesktop.impl.portal.PermissionStore"
                )
            )
            start("frontend", [str(build / "desktop-portal/xdg-desktop-portal")])
            wait_for(lambda: bus.name_has_owner("org.freedesktop.portal.Desktop"))
            interface = xdp.get_portal_iface(bus, "Language")

            def create_session(connection, token):
                return xdp.Request(
                    connection, xdp.get_portal_iface(connection, "Language")
                ).call(
                    "CreateSession",
                    parent_window="",
                    use_case=use_case,
                    instructions="",
                    options={
                        "session_handle_token": dbus.String(token, variant_level=1)
                    },
                )

            denied = create_session(bus, "denied")
            assert denied is not None and denied.response == 2, denied
            assert "PermissionDenied" in str(denied.results), denied
            assert management("aileron.Sessions.ListActive")["sessions"] == []
            print(
                "PASS: persisted permission denial reaches public D-Bus; no daemon session"
            )

            management(
                "aileron.Permissions.SetAppPermission",
                app_id=app_id,
                use_case=use_case,
                allowed=True,
            )
            allowed = create_session(bus, "allowed")
            assert allowed is not None and allowed.response == 0, allowed
            session = xdp.Session.from_response(bus, allowed)
            sessions = management("aileron.Sessions.ListActive")["sessions"]
            assert len(sessions) == 1 and sessions[0]["app_id"] == app_id, sessions
            tokens = []
            match = bus.add_signal_receiver(
                lambda *event: tokens.append(event),
                signal_name="TokenReceived",
                dbus_interface="org.freedesktop.portal.Language",
                bus_name="org.freedesktop.portal.Desktop",
                path="/org/freedesktop/portal/desktop",
            )
            stack.callback(match.remove)
            request = xdp.Request(bus, interface)
            response = request.call(
                "StreamResponse",
                session_handle=session.handle,
                input_json='[{"type":"input_text","text":"hello stack"}]',
                media_fds=dbus.Array([], signature="h"),
                options={},
            )
            assert response is not None and response.response == 0, response
            assert tokens and tokens[-1][3], tokens
            assert all(
                str(event[0]) == request.handle and str(event[1]) == str(session.handle)
                for event in tokens
            ), tokens
            assert "hello stack" in "".join(str(event[2]) for event in tokens), tokens

            if args.check_cancellation:
                # Stop only our own container's stub, then wait for bytes in its
                # input pipe. Cancellation must happen after the daemon sent work.
                children = [daemon.pid]
                stub_pid = None
                while children:
                    pid = children.pop()
                    proc = Path(f"/proc/{pid}")
                    if os.path.samefile(proc / "exe", rootfs / "entrypoint"):
                        stub_pid = pid
                        break
                    for task in (proc / "task").iterdir():
                        children.extend(
                            int(child)
                            for child in (task / "children").read_text().split()
                        )
                assert stub_pid is not None, "cannot locate the harness-owned OCI stub"
                pidfd = os.pidfd_open(stub_pid)
                stack.callback(os.close, pidfd)

                def resume_stub():
                    try:
                        signal.pidfd_send_signal(pidfd, signal.SIGCONT)
                    except ProcessLookupError:
                        pass

                stack.callback(resume_stub)
                signal.pidfd_send_signal(pidfd, signal.SIGSTOP)
                pipe = os.open(f"/proc/{stub_pid}/fd/0", os.O_RDONLY | os.O_NONBLOCK)
                stack.callback(os.close, pipe)
                cancelled = xdp.Request(bus, interface)
                close_errors = []
                replies = []

                def queued_at_runtime():
                    queued = array.array("i", [0])
                    fcntl.ioctl(pipe, termios.FIONREAD, queued)
                    return bool(queued[0] and replies) or bool(close_errors)

                interface.StreamResponse(
                    session.handle,
                    '[{"type":"input_text","text":"cancel this request"}]',
                    dbus.Array([], signature="h"),
                    {"handle_token": cancelled.handle_token},
                    reply_handler=lambda handle: replies.append(str(handle)),
                    error_handler=close_errors.append,
                )
                xdp.wait_for(queued_at_runtime)
                assert not close_errors and replies == [cancelled.handle], (
                    close_errors,
                    replies,
                )
                # The upstream Request.close helper waits for a mock-only signal.
                cancelled.request_interface.Close(timeout=5)
                exited = select.poll()
                exited.register(pidfd, select.POLLIN)
                assert exited.poll(
                    5000
                ), "Request.Close did not terminate the active stub"
                xdp.wait(100)
                assert cancelled.response is None, cancelled.response
                assert not any(
                    str(event[0]) == cancelled.handle for event in tokens
                ), tokens
                assert len(management("aileron.Sessions.ListActive")["sessions"]) == 1
                print(
                    "PASS: public Request.Close cancels queued daemon work and terminates the stub"
                )
            else:
                print("NOT RUN: in-flight cancellation; use --check-cancellation")

            session.session_interface.Close(timeout=5)
            wait_for(
                lambda: management("aileron.Sessions.ListActive")["sessions"] == []
            )
            print(
                "PASS: permission grant, real OCI stub streaming and explicit session cleanup"
            )

            owner = dbus.bus.BusConnection(env["DBUS_SESSION_BUS_ADDRESS"])
            stack.callback(owner.close)
            disconnected = create_session(owner, "disconnect")
            assert disconnected is not None and disconnected.response == 0, disconnected
            assert len(management("aileron.Sessions.ListActive")["sessions"]) == 1
            owner.close()
            wait_for(
                lambda: management("aileron.Sessions.ListActive")["sessions"] == []
            )
            print("PASS: public client disconnect removes real daemon session")

            revoked_sessions = []
            for token in ("revoke_first", "revoke_second"):
                response = create_session(bus, token)
                assert response is not None and response.response == 0, response
                revoked_sessions.append(xdp.Session.from_response(bus, response))
            assert len(management("aileron.Sessions.ListActive")["sessions"]) == 2
            management(
                "aileron.Permissions.SetAppPermission",
                app_id=app_id,
                use_case=use_case,
                allowed=False,
            )
            assert management("aileron.Sessions.ListActive")["sessions"] == []
            persisted = json.loads((data / "permissions.json").read_text())
            assert persisted[f"{app_id}/{use_case}"]["allowed"] is False
            denied = create_session(bus, "revoked_permission")
            assert denied is not None and denied.response == 2, denied
            assert "PermissionDenied" in str(denied.results), denied

            # A new grant must not restore sessions invalidated by revocation.
            management(
                "aileron.Permissions.SetAppPermission",
                app_id=app_id,
                use_case=use_case,
                allowed=True,
            )
            for revoked in revoked_sessions:
                request = xdp.Request(bus, interface)
                response = request.call(
                    "StreamResponse",
                    session_handle=revoked.handle,
                    input_json='[{"type":"input_text","text":"revoked session"}]',
                    media_fds=dbus.Array([], signature="h"),
                    options={},
                )
                assert response is not None and response.response == 2, response
                assert "SessionNotFound" in str(response.results), response
                assert not any(str(event[0]) == request.handle for event in tokens)
            assert management("aileron.Sessions.ListActive")["sessions"] == []
            response = create_session(bus, "regranted")
            assert response is not None and response.response == 0, response
            assert len(management("aileron.Sessions.ListActive")["sessions"]) == 1
            xdp.Session.from_response(bus, response).session_interface.Close(timeout=5)
            wait_for(
                lambda: management("aileron.Sessions.ListActive")["sessions"] == []
            )
            print(
                "PASS: persisted revocation invalidates existing sessions; "
                "regrant permits new sessions but does not revive old handles"
            )
        except BaseException:
            for name, _, log in processes:
                log.flush()
                log.seek(0)
                print(f"\n--- {name} ---\n{log.read()}", file=sys.stderr)
            raise


if __name__ == "__main__":
    main()
