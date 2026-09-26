"""Run the real portal frontend, backend, daemon and runtime on a private bus."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid

import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

from check_daemon import isolated_environment

BUS="org.freedesktop.portal.Desktop"
PATH="/org/freedesktop/portal/desktop"
LANGUAGE="org.freedesktop.portal.Language"
REQUEST="org.freedesktop.portal.Request"


def plain(value):
    if isinstance(value,dict): return {str(k):plain(v) for k,v in value.items()}
    if isinstance(value,(list,tuple)): return [plain(v) for v in value]
    if isinstance(value,dbus.Boolean): return bool(value)
    if isinstance(value,int): return int(value)
    if isinstance(value,str): return str(value)
    return value


def call_request(bus, proxy, method, args, options=None):
    token="check_"+uuid.uuid4().hex
    path=PATH+"/request/"+bus.get_unique_name()[1:].replace(".","_")+"/"+token
    options=dict(options or {})
    options["handle_token"]=dbus.String(token,variant_level=1)
    outcome=[]
    loop=GLib.MainLoop()
    def response(code,results):
        outcome.append((int(code),plain(results)));loop.quit()
    match=bus.add_signal_receiver(response,signal_name="Response",dbus_interface=REQUEST,path=path,bus_name=BUS)
    timeout=GLib.timeout_add_seconds(180,lambda: loop.quit() or GLib.SOURCE_REMOVE)
    try:
        returned=getattr(proxy,method)(*args,dbus.Dictionary(options,signature="sv"))
        if str(returned)!=path: raise AssertionError("unexpected request handle")
        if not outcome: loop.run()
        if not outcome: raise TimeoutError("portal request timed out")
        code,results=outcome[0]
        if code: raise RuntimeError(f"portal request failed: {code}, {results}")
        return path,results
    finally:
        GLib.source_remove(timeout)
        match.remove()


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--frontend",type=Path,required=True)
    parser.add_argument("--backend",type=Path,required=True)
    parser.add_argument("--daemon",type=Path,required=True)
    parser.add_argument("--image",required=True)
    parser.add_argument("--oci-store",type=Path,required=True)
    parser.add_argument("--model-dir",type=Path,required=True)
    parser.add_argument("--library-path",type=Path)
    parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args()
    args.output.parent.mkdir(parents=True,exist_ok=True)
    DBusGMainLoop(set_as_default=True)
    report={"checks":[]}
    with tempfile.TemporaryDirectory(prefix="portal-check-",dir=args.output.parent.resolve()) as root:
        root=Path(root)
        env,address=isolated_environment(root,args.image,args.model_dir)
        for name in ["portals","config","data/applications"]: (root/name).mkdir(parents=True,exist_ok=True)
        (root/"portals/aileron.portal").write_text("[portal]\nDBusName=org.freedesktop.impl.portal.desktop.aileron\nInterfaces=org.freedesktop.impl.portal.Language;\n")
        (root/"portals/ailerontest-portals.conf").write_text("[preferred]\ndefault=aileron;\n")
        (root/"data/applications/org.aileron.Conformance.desktop").write_text("[Desktop Entry]\nType=Application\nName=Aileron conformance\nExec=python3\n")
        env.update(XDG_DATA_HOME=str(root/"data"),XDG_DATA_DIRS=str(root/"data"),
                   XDG_CONFIG_HOME=str(root/"config"),XDG_CONFIG_DIRS=str(root/"config"),
                   XDG_DESKTOP_PORTAL_DIR=str(root/"portals"),XDG_CURRENT_DESKTOP="AileronTest",
                   GSETTINGS_SCHEMA_DIR="/usr/share/glib-2.0/schemas",GSETTINGS_BACKEND="memory")
        if args.library_path: env["LD_LIBRARY_PATH"]=str(args.library_path.resolve())
        bus_process=subprocess.Popen(["dbus-daemon","--session","--nofork","--print-address=1",
            "--address=unix:path="+str(root/"run/bus")],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
        env["DBUS_SESSION_BUS_ADDRESS"]=bus_process.stdout.readline().strip()
        children=[]
        bus=None
        with args.output.with_suffix(".log").open("w") as log:
            try:
                children.append(subprocess.Popen([str(args.daemon.resolve()),"--allow-all","--oci-store",str(args.oci_store.resolve())],env=env,stdout=log,stderr=log))
                deadline=time.monotonic()+20
                while not address.exists():
                    if time.monotonic()>deadline: raise RuntimeError("daemon did not start")
                    time.sleep(0.05)
                children.append(subprocess.Popen([str(args.backend.resolve())],env=env,stdout=log,stderr=log))
                bus=dbus.bus.BusConnection(env["DBUS_SESSION_BUS_ADDRESS"])
                deadline=time.monotonic()+20
                while not bus.name_has_owner("org.freedesktop.impl.portal.desktop.aileron"):
                    if time.monotonic()>deadline: raise RuntimeError("backend did not start")
                    time.sleep(0.05)
                children.append(subprocess.Popen([str(args.frontend.resolve())],env=env,stdout=log,stderr=log))
                deadline=time.monotonic()+20
                while not bus.name_has_owner(BUS):
                    if time.monotonic()>deadline: raise RuntimeError("frontend did not start")
                    time.sleep(0.05)
                desktop=bus.get_object(BUS,PATH)
                dbus.Interface(desktop,"org.freedesktop.host.portal.Registry").Register("org.aileron.Conformance",dbus.Dictionary({},signature="sv"))
                proxy=dbus.Interface(desktop,LANGUAGE)
                _,created=call_request(bus,proxy,"CreateSession",("","language.analyze","Answer clearly."))
                session=dbus.ObjectPath(created["session_handle"])
                _,caps=call_request(bus,proxy,"GetReasoningCapabilities",(session,))
                report["capabilities"]=caps
                report["checks"].append("public capability discovery")
                tokens=[];reasoning=[]
                token_match=bus.add_signal_receiver(lambda request,owner,text,done: tokens.append(str(text)),
                    signal_name="TokenReceived",dbus_interface=LANGUAGE,path=PATH,bus_name=BUS)
                reasoning_match=bus.add_signal_receiver(lambda request,owner,text: reasoning.append(str(text)),
                    signal_name="ReasoningReceived",dbus_interface=LANGUAGE,path=PATH,bus_name=BUS)
                try:
                    thinking="off" if "off" in caps["thinking_modes"] else "auto"
                    _,metadata=call_request(bus,proxy,"StreamResponse",(session,
                        json.dumps([{"type":"input_text","text":"What is 2 + 2? Give a short answer."}]),dbus.Array([],signature="h")),{
                            "thinking":dbus.String(thinking,variant_level=1),"maximum_response_tokens":dbus.Int64(128,variant_level=1)})
                    report["answer"]="".join(tokens);report["completion"]=metadata
                    if "4" not in report["answer"] or reasoning or "usage" not in metadata:
                        raise AssertionError("public answer, privacy, or completion metadata failed")
                    report["checks"].append("real public generation and completion metadata")
                    if "on" in caps["thinking_modes"]:
                        tokens.clear();reasoning.clear()
                        _,metadata=call_request(bus,proxy,"StreamResponse",(session,
                            json.dumps([{"type":"input_text","text":"Reason carefully: a snail climbs 3 meters each day and slips 2 each night. When does it escape a 10-meter well?"}]),dbus.Array([],signature="h")),{
                                "thinking":dbus.String("on",variant_level=1),"include_reasoning":dbus.Boolean(True,variant_level=1),
                                "temperature":dbus.Double(0.6,variant_level=1),"maximum_response_tokens":dbus.Int64(128,variant_level=1)})
                        if not reasoning or metadata["usage"]["completion_tokens"]>128:
                            raise AssertionError("public reasoning events or shared budget failed")
                        report["reasoning"]="".join(reasoning);report["reasoning_completion"]=metadata
                        report["checks"].append("real public reasoning and bounded completion")
                finally:
                    token_match.remove();reasoning_match.remove()
                print(report["checks"])
            except BaseException as error:
                report["error"]=str(error)
                raise
            finally:
                args.output.write_text(json.dumps(report,indent=2)+"\n")
                if bus: bus.close()
                for process in reversed(children):
                    process.terminate()
                    try: process.wait(timeout=5)
                    except subprocess.TimeoutExpired: process.kill();process.wait()
                # Only this fixture's private crun namespace is inspected/removed.
                listed=subprocess.run(["crun","list","--format","json"],env=env,capture_output=True,text=True)
                for item in json.loads(listed.stdout or "[]"):
                    subprocess.run(["crun","delete","--force",item["id"]],env=env,capture_output=True)
                    bundle=Path(item["bundle"])
                    if bundle.parent==args.oci_store.resolve()/"bundles": shutil.rmtree(bundle,ignore_errors=True)
                bus_process.terminate();bus_process.wait(timeout=5)


if __name__=="__main__": main()
