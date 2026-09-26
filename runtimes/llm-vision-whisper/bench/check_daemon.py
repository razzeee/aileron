"""Exercise real generation v2 and tool continuation through an isolated daemon.

Requires an exported runtime rootfs and a local tool-capable model. This does
not use the user's daemon socket, assignments, permissions, or profile store.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import threading
import queue
import time


def rpc(address, method, parameters, more=False, timeout=180, on_response=None):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect(str(address))
        connection.sendall(json.dumps({"method":method,"parameters":parameters,"more":more}).encode()+b"\0")
        pending = b""
        responses = []
        while True:
            data = connection.recv(65536)
            if not data:
                raise RuntimeError("Varlink closed before a terminal response")
            pending += data
            while b"\0" in pending:
                message, pending = pending.split(b"\0",1)
                response = json.loads(message)
                responses.append(response)
                if on_response:
                    on_response(response)
                if not response.get("continues",False):
                    return responses


def success(responses):
    for response in responses:
        if "error" in response:
            raise RuntimeError(str(response))
    return responses[-1].get("parameters",{})


def isolated_environment(directory, image, model_dir):
    data=directory/"data"/"aileron"
    (data/"profiles").mkdir(parents=True)
    runtime=directory/"run"
    runtime.mkdir(mode=0o700)
    profile={"profile_id":"check","model_id":model_dir.name,"runtime_id":"llm-vision-whisper",
              "runtime_options":{"N_THREADS":"2","N_CTX":"4096"},
             "artifact_path":str(model_dir.resolve()),"runtime_images":[{"variant":"cpu","image_ref":image}],
             "use_cases":["language.analyze"],"installed_at":"isolated-test","source":"user"}
    (data/"profiles"/"check.json").write_text(json.dumps(profile))
    (data/"assignments.json").write_text(json.dumps({"language.analyze":"check"}))
    manifests=directory/"manifests"
    (manifests/"runtimes").mkdir(parents=True)
    (manifests/"runtimes"/"llm-vision-whisper.json").write_text(json.dumps({
        "runtime_id":"llm-vision-whisper","images":{"cpu":image}}))
    env={**os.environ,"AILERON_DATA_HOME":str(directory/"data"),"AILERON_SYSTEM_DATA_DIR":str(directory/"system"),
         "AILERON_MANIFEST_DIRS":str(manifests),"XDG_RUNTIME_DIR":str(runtime)}
    return env,runtime/"aileron.socket"


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon",type=Path,required=True)
    parser.add_argument("--image",required=True)
    parser.add_argument("--oci-store",type=Path,required=True)
    parser.add_argument("--model-dir",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--skip-reasoning",action="store_true")
    parser.add_argument("--lifecycle-only",action="store_true")
    args=parser.parse_args()
    args.output.parent.mkdir(parents=True,exist_ok=True)
    report={"checks":[]}
    with tempfile.TemporaryDirectory(prefix="aileron-check-",dir=args.output.parent.resolve()) as directory:
        directory=Path(directory)
        env,address=isolated_environment(directory,args.image,args.model_dir)
        sessions=[]
        with args.output.with_suffix(".log").open("w") as log:
            daemon=subprocess.Popen([str(args.daemon.resolve()),"--allow-all","--oci-store",str(args.oci_store.resolve())],env=env,stdout=log,stderr=log)
            try:
                deadline=time.monotonic()+20
                while not address.exists():
                    if daemon.poll() is not None or time.monotonic()>deadline:
                        raise RuntimeError("daemon did not start; inspect log")
                    time.sleep(0.05)
                for _ in range(2):
                    created=success(rpc(address,"aileron.Inference.CreateSession",{
                        "app_id":"org.aileron.Conformance","use_case":"language.analyze",
                        "instructions":"Use provided tools for external information. Never invent calendar events."}))
                    sessions.append(created["session_id"])
                before={item["id"] for item in json.loads(subprocess.check_output(["crun","list","--format","json"],env=env))}
                prepared=queue.Queue()
                def prepare():
                    try:
                        prepared.put(rpc(address,"aileron.Inference.GetReasoningCapabilities",{"session_id":sessions[0]}))
                    except BaseException as error:
                        prepared.put(error)
                threading.Thread(target=prepare,daemon=True).start()
                deadline=time.monotonic()+10
                while True:
                    records=json.loads(subprocess.check_output(["crun","list","--format","json"],env=env,stderr=subprocess.DEVNULL))
                    if any(item["id"] not in before and str(item.get("bundle", "")).startswith(str(args.oci_store.resolve())) for item in records):
                        break
                    if time.monotonic()>deadline:
                        raise AssertionError("cold-start container did not appear")
                    time.sleep(0.01)
                started=time.monotonic()
                success(rpc(address,"aileron.Inference.CancelActiveRequest",{"session_id":sessions[0]},timeout=5))
                cancelled=prepared.get(timeout=2)
                report["startup_cancel_seconds"]=time.monotonic()-started
                if isinstance(cancelled,BaseException) or cancelled[-1].get("error")!="aileron.Inference.RequestCancelled":
                    raise AssertionError(f"cold-start cancellation failed: {cancelled}")
                report["checks"].append("cold-start cancellation preserves the session for later requests")
                caps=success(rpc(address,"aileron.Inference.GetReasoningCapabilities",{"session_id":sessions[0]}))
                report["capabilities"]=caps
                running=threading.Event()
                completed=queue.Queue()
                def generate_until_cancelled():
                    try:
                        completed.put(rpc(address,"aileron.Inference.StreamResponse2",{
                            "session_id":sessions[0],"input_json":json.dumps([{"type":"input_text","text":"Write a very long detailed story about exploring a distant planet."}]),
                            "media_paths":[],"options":{"maximum_response_tokens":1024,"temperature":0.0}},True,
                            on_response=lambda reply: running.set() if reply.get("parameters",{}).get("event",{}).get("kind")=="answer" else None))
                    except BaseException as error:
                        completed.put(error)
                threading.Thread(target=generate_until_cancelled,daemon=True).start()
                if not running.wait(120):
                    raise AssertionError("generation did not start")
                started=time.monotonic()
                success(rpc(address,"aileron.Inference.CancelActiveRequest",{"session_id":sessions[0]},timeout=5))
                cancelled=completed.get(timeout=2)
                report["active_cancel_seconds"]=time.monotonic()-started
                if isinstance(cancelled,BaseException) or cancelled[-1].get("error")!="aileron.Inference.RequestCancelled":
                    raise AssertionError(f"active cancellation failed: {cancelled}")
                success(rpc(address,"aileron.Inference.GetReasoningCapabilities",{"session_id":sessions[0]}))
                report["checks"].append("active cancellation is classified and the same session can be reused")
                if args.lifecycle_only:
                    print(report["checks"])
                    return
                options={"maximum_response_tokens":256,"temperature":0.0}
                if "off" in caps["capabilities"]["thinking_modes"]:
                    options["thinking"]="off"
                fields=[{"name":"answer","kind":"string","description":"The answer based on the tool result","required":True}]
                tools=[{"name":"calendar_lookup","description":"Look up calendar events for a date.",
                        "schema_json":json.dumps({"type":"object","required":["date"],"properties":{"date":{"type":"string"}}})}]
                initial={"session_id":sessions[0],"prompt":"Look up my calendar for 2026-09-24 before answering. Use calendar_lookup.",
                         "media_paths":[],"fields":fields,"tools":tools,"options":options}
                responses=rpc(address,"aileron.Inference.StreamRespondGuided2",initial,True)
                report["initial"]=responses
                success(responses)
                calls=[call for response in responses for call in response["parameters"]["event"].get("tool_calls",[]) or []]
                if not calls:
                    raise AssertionError("model did not request a tool")
                report["checks"].append("native tool calls")
                results=[{"id":call["id"],"content":"Team sync at 10:00.","content_json":""} for call in calls]
                continuation={**initial,"results":results,"prompt":"Use the supplied calendar result to answer. Do not call the tool again."}
                crossed=rpc(address,"aileron.Inference.StreamSubmitToolResultsGuided2",{**continuation,"session_id":sessions[1]},True)
                if crossed[-1].get("error") != "aileron.Inference.InvalidInput":
                    raise AssertionError(f"cross-session results accepted: {crossed}")
                report["checks"].append("cross-session tool results rejected")
                responses=rpc(address,"aileron.Inference.StreamSubmitToolResultsGuided2",continuation,True)
                report["continuation"]=responses
                success(responses)
                snapshots=[response["parameters"]["event"]["snapshot_json"] for response in responses
                           if response["parameters"]["event"].get("snapshot_json")]
                if not snapshots or "10" not in json.loads(snapshots[-1])["answer"]:
                    raise AssertionError("final answer did not use the supplied tool result")
                final=responses[-1]["parameters"]["event"]
                if final["kind"]!="completed" or final["usage"]["completion_tokens"]>256:
                    raise AssertionError("completion metadata or shared token budget invalid")
                report["checks"].append("schema final answer and bounded aggregate usage")
                replay=rpc(address,"aileron.Inference.StreamSubmitToolResultsGuided2",continuation,True)
                if replay[-1].get("error") != "aileron.Inference.InvalidInput":
                    raise AssertionError("completed tool results could be replayed")
                report["checks"].append("completed tool results cannot be replayed")
                modes=caps["capabilities"]["thinking_modes"]
                if not args.skip_reasoning and "on" in modes and caps["capabilities"]["reasoning_output"]:
                    for thinking, include in [("on",True),("on",False),("off",True)]:
                        if thinking not in modes:
                            continue
                        responses=rpc(address,"aileron.Inference.StreamResponse2",{
                            "session_id":sessions[0],"input_json":json.dumps([{"type":"input_text","text":"Reason carefully about this puzzle before answering: a snail climbs 3 meters each day and slips 2 meters each night. How many days does it take to escape a 10-meter well? Check the final day separately, then give a short final answer."}]),
                            "media_paths":[],"options":{"thinking":thinking,"include_reasoning":include,"temperature":0.6,"maximum_response_tokens":512}},True)
                        report[f"reasoning_{thinking}_{include}"]=responses
                        success(responses)
                        events=[response["parameters"]["event"] for response in responses]
                        has_reasoning=any(event["kind"]=="reasoning" and event.get("text") for event in events)
                        if has_reasoning != (thinking=="on" and include):
                            raise AssertionError("thinking mode or reasoning opt-in was not honored")
                        if not any(event["kind"]=="answer" and event.get("text") for event in events):
                            if events[-1].get("finish_reason") != "length":
                                raise AssertionError("missing final answer was not reported as output exhaustion")
                        if events[-1]["usage"]["completion_tokens"]>512:
                            raise AssertionError("reasoning exceeded the total output budget")
                    simple=rpc(address,"aileron.Inference.StreamResponse2",{
                        "session_id":sessions[0],"input_json":json.dumps([{"type":"input_text","text":"What is 2 + 2? Give a short answer."}]),
                        "media_paths":[],"options":{"thinking":"on","include_reasoning":True,"temperature":0.0,"maximum_response_tokens":128}},True)
                    success(simple)
                    report["reasoning_simple"]=simple
                    if not any(r["parameters"]["event"]["kind"]=="answer" and r["parameters"]["event"].get("text") for r in simple):
                        raise AssertionError("simple thinking-enabled request produced no answer")
                    report["checks"].append("thinking on/off, reasoning opt-in, final answers, and output exhaustion metadata")
                print(report["checks"])
            except BaseException as error:
                report["error"]=str(error)
                raise
            finally:
                args.output.write_text(json.dumps(report,indent=2)+"\n")
                for session in sessions:
                    try:
                        rpc(address,"aileron.Sessions.KillSession",{"session_id":session},timeout=10)
                    except (OSError,RuntimeError):
                        pass
                daemon.send_signal(signal.SIGINT)
                try:
                    daemon.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    daemon.kill();daemon.wait()


if __name__=="__main__":
    main()
