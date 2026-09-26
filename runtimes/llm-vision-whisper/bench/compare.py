"""Compare two packaged runtimes over their actual JSON-lines interface.

No model downloads or host service changes. Images and model directories must
already exist. Raw events are retained so semantic outcomes can be inspected.
"""

import argparse
from collections import deque
import hashlib
import json
import math
from pathlib import Path
import queue
import statistics
import subprocess
import threading
import time
import uuid


def command_json(*args):
    return json.loads(subprocess.check_output(args, text=True))


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def percentile(values, fraction):
    values = sorted(values)
    return values[max(0, math.ceil(len(values) * fraction) - 1)] if values else None


def cosine(a, b):
    if len(a) != len(b) or not a:
        raise ValueError("embedding dimensions differ or are empty")
    denominator = math.sqrt(math.fsum(x*x for x in a) * math.fsum(x*x for x in b))
    if denominator == 0:
        raise ValueError("zero-norm embedding")
    return math.fsum(x*y for x, y in zip(a, b)) / denominator


def check_result(events, expectation):
    terminal = events[-1]
    error = terminal.get("error")
    kind = expectation["kind"]
    if kind == "error":
        if error != expectation["code"]:
            raise ValueError(f"expected {expectation['code']}, got {error}")
        return
    if error:
        raise ValueError(f"runtime returned {error}: {terminal.get('reason')}")
    if kind == "text":
        text = "".join(event.get("token", "") for event in events)
        if not text.strip() and not expectation.get("allow_empty", False):
            raise ValueError("empty answer")
        if any(fragment.casefold() not in text.casefold() for fragment in expectation.get("contains_all", [])):
            raise ValueError("answer misses fixture content")
        if any(fragment.casefold() in text.casefold() for fragment in expectation.get("forbid_any", [])):
            raise ValueError("answer contradicts fixture input")
        if expectation.get("contains_any") and not any(fragment.casefold() in text.casefold() for fragment in expectation["contains_any"]):
            raise ValueError("answer misses the requested response")
        if "equals" in expectation and text.strip()!=expectation["equals"]:
            raise ValueError("answer does not match the fixed-output fixture")
    elif kind == "json":
        value = json.loads(terminal.get("result", terminal.get("snapshot", "")))
        for key in expectation.get("required", []):
            if not isinstance(value, dict) or key not in value:
                raise ValueError(f"missing JSON field: {key}")
        for key,expected in expectation.get("equals",{}).items():
            if not isinstance(value,dict) or value.get(key)!=expected:
                raise ValueError(f"incorrect extracted value: {key}")
        for event in events:
            if "snapshot" in event:
                json.loads(event["snapshot"])
    elif kind == "embedding":
        vector = terminal.get("embedding", [])
        if not vector or not all(type(x) in (int, float) and math.isfinite(x) for x in vector):
            raise ValueError("invalid embedding")
    elif kind == "detections":
        value=json.loads(terminal.get("result",""))
        detections=value.get("detections") if isinstance(value,dict) else None
        if not isinstance(detections,list) or set(value)!={"detections"}:
            raise ValueError("invalid detection envelope")
        keys={"label","confidence","x","y","width","height"}
        for detection in detections:
            if not isinstance(detection,dict) or set(detection)!=keys or not isinstance(detection["label"],str):
                raise ValueError("invalid detection fields")
            if not all(type(detection[key]) in (int,float) and 0<=detection[key]<=1 for key in keys-{"label"}):
                raise ValueError("detection values are not normalized")
    elif kind == "tools":
        calls = terminal.get("tool_calls", [])
        if not calls:
            raise ValueError("no tool calls")
        for call in calls:
            if not call.get("id") or call["name"] not in expectation["names"]:
                raise ValueError("unexpected tool call")
            if not isinstance(json.loads(call["arguments_json"]), dict):
                raise ValueError("tool arguments are not an object")
    elif kind == "capabilities":
        caps=terminal.get("capabilities",{})
        for key in ("thinking_modes","reasoning_efforts"):
            if key in expectation and caps.get(key)!=expectation[key]:
                raise ValueError(f"unexpected {key}: {caps.get(key)}")
    elif kind == "reasoning":
        answer="".join(event.get("token","") for event in events)
        trace="".join(event.get("reasoning","") for event in events)
        if expectation.get("trace")=="required" and not trace:
            raise ValueError("missing reasoning events")
        if expectation.get("trace")=="forbidden" and trace:
            raise ValueError("reasoning emitted without opt-in")
        count=terminal.get("usage",{}).get("completion_tokens")
        if type(count) is not int or not 0<=count<=expectation["max_tokens"]:
            raise ValueError("invalid output-token accounting")
        if terminal.get("finish_reason") not in ("stop","length"):
            raise ValueError("missing completion classification")
        if terminal["finish_reason"]=="stop" and not answer.strip():
            raise ValueError("normal completion has no final answer")
        if any(fragment.casefold() not in answer.casefold() for fragment in expectation.get("contains_all", [])):
            raise ValueError("final answer misses fixture content")
        if any(marker in answer for marker in ("<think>","<|channel>thought","<|channel|>analysis","<|meta_sep|>analysis")):
            raise ValueError("reasoning markers leaked into the answer")
    else:
        raise ValueError(f"unknown expectation: {kind}")


class Session:
    def __init__(self, command, timeout=120, stop=None):
        self.stop = stop
        self.timeout = timeout
        self.stderr = deque(maxlen=200)
        self.output = queue.Queue()
        self.ready = threading.Event()
        self.started = time.monotonic()
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, text=True, bufsize=1)
        self.readers = [threading.Thread(target=self._stderr, daemon=True),
                        threading.Thread(target=self._stdout, daemon=True)]
        for reader in self.readers:
            reader.start()
        try:
            while not self.ready.wait(0.01):
                if self.process.poll() is not None or time.monotonic() - self.started > timeout:
                    raise RuntimeError("runtime failed before readiness: " + "".join(self.stderr))
            self.ready_seconds = time.monotonic() - self.started
        except BaseException:
            self.close()
            raise

    def _stderr(self):
        for line in self.process.stderr:
            self.stderr.append(line)
            if line.strip().endswith("] ready"):
                self.ready.set()

    def _stdout(self):
        for line in self.process.stdout:
            self.output.put((time.monotonic(), line))
        self.output.put((time.monotonic(), None))

    def request(self, request):
        request = {**request, "id": str(uuid.uuid4())}
        started = time.monotonic()
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        events = []
        first_answer = None
        while True:
            remaining = self.timeout - (time.monotonic() - started)
            if remaining <= 0:
                raise TimeoutError("request did not terminate")
            received, line = self.output.get(timeout=remaining)
            if line is None:
                raise RuntimeError("runtime closed stdout before completion")
            event = json.loads(line)
            if event.get("id") != request["id"]:
                raise ValueError("response correlation mismatch")
            if event.get("token") and first_answer is None:
                first_answer = received - started
            events.append(event)
            if event.get("done"):
                usage = event.get("usage", {})
                # Chunk counts are not token counts. Missing native usage stays missing.
                tokens = usage.get("completion_tokens")
                elapsed = received - started
                return {"seconds": elapsed, "first_answer_seconds": first_answer,
                        "completion_tokens": tokens,
                        "tokens_per_second": tokens / elapsed if tokens is not None else None,
                        "events": events}

    def close(self):
        started = time.monotonic()
        if self.process.stdin and not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            if self.stop:
                self.stop()
            self.process.kill()
            self.process.wait(timeout=5)
        finally:
            if self.stop:
                self.stop()
            for reader in self.readers:
                reader.join(timeout=1)
            self.process.stdout.close()
            self.process.stderr.close()
        return time.monotonic() - started


def process_tree(pid):
    result = [pid]
    try:
        children = Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
        for child in children:
            result.extend(process_tree(int(child)))
    except (OSError, ValueError):
        pass
    return result


class Resources:
    def __init__(self, pid, track_gpu=False):
        self.pid = pid
        self.track_gpu = track_gpu
        self.gpu_before = self.gpu_memory() if track_gpu else None
        self.peaks = {}
        self.finished = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    @staticmethod
    def gpu_memory():
        paths=list(Path("/sys/class/drm").glob("card[0-9]*/device/mem_info_vram_used"))
        if not paths:
            return None
        try:
            return sum(int(path.read_text()) for path in paths)
        except (OSError,ValueError):
            return None

    def run(self):
        while not self.finished.is_set():
            sample = {"processes": 0, "threads": 0}
            pss = []
            high_water = []
            for pid in process_tree(self.pid):
                try:
                    status = Path(f"/proc/{pid}/status").read_text()
                    sample["processes"] += 1
                    sample["threads"] += int(next(line.split()[1] for line in status.splitlines()
                                                  if line.startswith("Threads:")))
                    high_water.append(int(next(line.split()[1] for line in status.splitlines()
                                               if line.startswith("VmHWM:"))) * 1024)
                    rollup = Path(f"/proc/{pid}/smaps_rollup").read_text()
                    pss.append(int(next(line.split()[1] for line in rollup.splitlines()
                                        if line.startswith("Pss:"))) * 1024)
                except (OSError, StopIteration, ValueError):
                    pass
            if len(pss) == sample["processes"] and pss:
                sample["pss_bytes"] = sum(pss)
            if len(high_water) == sample["processes"] and high_water:
                sample["rss_high_water_bytes"] = sum(high_water)
            try:
                group = next(line[3:] for line in Path(f"/proc/{self.pid}/cgroup").read_text().splitlines()
                             if line.startswith("0::"))
                root = Path("/sys/fs/cgroup") / group.lstrip("/")
                sample["cgroup_memory_bytes"] = int((root / "memory.current").read_text())
                sample["cgroup_memory_peak_bytes"] = int((root / "memory.peak").read_text())
                memory = dict(line.split() for line in (root / "memory.stat").read_text().splitlines())
                for key in ("anon", "file", "kernel"):
                    if key in memory:
                        sample[f"cgroup_{key}_bytes"] = int(memory[key])
            except (OSError, StopIteration, ValueError):
                pass
            for key, value in sample.items():
                self.peaks[key] = max(self.peaks.get(key, 0), value)
            if self.track_gpu:
                gpu=self.gpu_memory()
                if gpu is not None:
                    self.peaks["gpu_vram_bytes"]=max(self.peaks.get("gpu_vram_bytes",0),gpu)
            self.finished.wait(0.1)

    def close(self):
        self.finished.set()
        self.thread.join()
        if self.gpu_before is not None:
            self.peaks["gpu_vram_before_bytes"]=self.gpu_before
        return self.peaks


def run_image(image, model, workloads, backend, repetitions, context, threads, variant="cpu", memory="8g"):
    name = "aileron-compare-" + uuid.uuid4().hex
    entrypoint = None
    if backend == "reference":
        family = "vision" if (model / "mmproj.gguf").exists() else "llm"
        entrypoint = f"/usr/local/bin/aileron-runtime-{family}-llama-cpp"
    command = ["podman", "run", "--rm", "-i", "--name", name, "--network", "none",
               "--read-only", "--cap-drop=ALL", "--security-opt=no-new-privileges",
               # Match Aileron's crun configuration without relabeling user model files.
               "--security-opt=label=disable",
               "--pids-limit=256", f"--memory={memory}", "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,size=256m",
               "--mount", f"type=bind,source={model},destination=/model,ro=true",
               "--env", f"N_CTX={context}", "--env", f"N_THREADS={threads}",
               "--env", f"AILERON_DEVICE={variant}", "--env", f"N_GPU_LAYERS={0 if variant == 'cpu' else -1}"]
    if variant in ("vulkan", "rocm"):
        command += ["--device", "/dev/dri"]
    if variant == "rocm":
        command += ["--device", "/dev/kfd"]
    if variant == "cuda":
        command += ["--device", "nvidia.com/gpu=all"]
    if entrypoint:
        command += ["--entrypoint", entrypoint]
    command += [image]

    def stop():
        subprocess.run(["podman", "rm", "--force", "--ignore", name], capture_output=True, check=False)

    monitor = Resources(0, track_gpu=variant!="cpu")
    try:
        session = Session(command, stop=stop)
    except BaseException:
        monitor.close()
        raise
    results = []
    try:
        info = command_json("podman", "inspect", name)[0]
        monitor.pid = info["State"]["Pid"]
        if variant != "cpu" and any("no usable gpu" in line.lower() for line in session.stderr):
            raise RuntimeError("requested accelerator was unavailable; refusing a CPU fallback benchmark")
        for repetition in range(repetitions):
            for workload in workloads:
                item = {"backend": backend, "workload": workload["name"], "repetition": repetition,
                        "phase":"initial" if repetition == 0 else "warm"}
                try:
                    result = session.request(workload["request"])
                    item.update(result)
                    check_result(result["events"], workload["expect"])
                    item["passed"] = True
                except (ValueError, RuntimeError, TimeoutError, queue.Empty, BrokenPipeError) as error:
                    item.update(passed=False, error=str(error))
                results.append(item)
    finally:
        resources = monitor.close() if monitor else {}
        teardown = session.close()
    return {"backend": backend, "ready_seconds": session.ready_seconds,
            "teardown_seconds": teardown, "resources": resources, "results": results,
            "stderr": list(session.stderr)}


def report(data):
    lines = ["# Runtime comparison", "", "All raw events and provenance are in the JSON report.", "",
             "| Backend | Workload | Passed | Median seconds | p95 seconds |",
             "| --- | --- | --- | --- | --- |"]
    groups = {}
    for run in data["runs"]:
        for result in run["results"]:
            groups.setdefault((run["backend"], result["workload"]), []).append(result)
    for (backend, workload), results in sorted(groups.items()):
        warm = [r for r in results if r.get("phase", "warm") == "warm"]
        times = [r["seconds"] for r in warm if r["passed"]]
        median = f"{statistics.median(times):.6f}" if times else "unavailable"
        p95 = f"{percentile(times, 0.95):.6f}" if times else "unavailable"
        lines.append(f"| {backend} | {workload} | {sum(r['passed'] for r in results)}/{len(results)} | {median} | {p95} |")
    lines += ["", "## Warm latency gates", "", "| Workload | Median change | p95 change | Gate |",
              "| --- | --- | --- | --- |"]
    for workload in sorted({workload for _,workload in groups}):
        reference=[r["seconds"] for r in groups.get(("reference",workload),[]) if r["passed"] and r.get("phase","warm")=="warm"]
        candidate=[r["seconds"] for r in groups.get(("candidate",workload),[]) if r["passed"] and r.get("phase","warm")=="warm"]
        if not reference or not candidate:
            lines.append(f"| {workload} | unavailable | unavailable | Not comparable |")
            continue
        median=statistics.median(candidate)/statistics.median(reference)-1
        p95=percentile(candidate,0.95)/percentile(reference,0.95)-1
        status="pass" if median<=0.05 and p95<=0.10 else "FAIL"
        if min(len(reference),len(candidate))<20:
            status="insufficient samples"
        lines.append(f"| {workload} | {median:+.1%} | {p95:+.1%} | {status} |")
    lines += ["", "## Startup and resources", "", "| Backend | Median ready seconds | Peak PSS bytes | RSS high-water bytes | Peak cgroup bytes | GPU delta bytes |",
              "| --- | --- | --- | --- | --- | --- |"]
    for backend in ["reference","candidate"]:
        runs=[r for r in data["runs"] if r["backend"]==backend and "ready_seconds" in r]
        ready=[r["ready_seconds"] for r in runs]
        pss=[r["resources"]["pss_bytes"] for r in runs if "pss_bytes" in r["resources"]]
        memory=[r["resources"]["cgroup_memory_peak_bytes"] for r in runs if "cgroup_memory_peak_bytes" in r["resources"]]
        hwm=[r["resources"]["rss_high_water_bytes"] for r in runs if "rss_high_water_bytes" in r["resources"]]
        gpu=[r["resources"]["gpu_vram_bytes"]-r["resources"]["gpu_vram_before_bytes"] for r in runs
             if "gpu_vram_bytes" in r["resources"] and "gpu_vram_before_bytes" in r["resources"]]
        lines.append(f"| {backend} | {statistics.median(ready) if ready else 'unavailable'} | {max(pss) if pss else 'unavailable'} | {max(hwm) if hwm else 'unavailable'} | {max(memory) if memory else 'unavailable'} | {max(gpu) if gpu else 'unavailable'} |")
    embeddings = {}
    for (backend, workload), results in groups.items():
        for result in results:
            if result["passed"] and result.get("events") and "embedding" in result["events"][-1]:
                embeddings[(backend, workload)] = result["events"][-1]["embedding"]
                break
    if embeddings:
        lines += ["", "## Embedding agreement", "", "| Workload | Cosine | Gate |", "| --- | --- | --- |"]
        for backend, workload in sorted(embeddings):
            if backend == "reference" and ("candidate", workload) in embeddings:
                try:
                    similarity = cosine(embeddings[(backend, workload)], embeddings[("candidate", workload)])
                    lines.append(f"| {workload} | {similarity:.9f} | {'pass' if similarity >= 0.9999 else 'FAIL'} |")
                except ValueError as error:
                    lines.append(f"| {workload} | {error} | FAIL |")
    lines += ["", "Missing token usage is not estimated from chunk counts. GPU telemetry, when present, is whole-device AMD VRAM usage with a pre-start baseline; other devices remain unmeasured.",
              "PSS sampling starts after readiness; RSS high-water and cgroup memory.peak retain startup peaks. Resources are sampled every 100 ms. Initial requests are excluded from warm latency statistics.",
              "This report records measurements; acceptance requires the full design's conformance and comparison gates."]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-image", required=True)
    parser.add_argument("--candidate-image", required=True)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--workloads", type=Path, default=Path(__file__).with_name("text.json"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cold-runs", type=int, default=5)
    parser.add_argument("--warm-runs", type=int, default=20)
    parser.add_argument("--context", type=int, default=4096)
    parser.add_argument("--threads", type=int, default=2)
    parser.add_argument("--variant", choices=["cpu","vulkan","cuda","rocm"], default="cpu")
    parser.add_argument("--memory", default="8g")
    args = parser.parse_args()
    if min(args.cold_runs, args.warm_runs, args.context, args.threads) < 1:
        parser.error("run counts, context and threads must be positive")
    model = args.model_dir.resolve(strict=True)
    workloads = json.loads(args.workloads.read_text())
    images = {backend: command_json("podman", "image", "inspect", image)[0]
              for backend, image in [("reference", args.reference_image), ("candidate", args.candidate_image)]}
    for backend,image in images.items():
        if image.get("Config",{}).get("Labels",{}).get("org.aileron.variant") != args.variant:
            parser.error(f"{backend} image is not labelled for variant {args.variant}")
    data = {"schema_version":2,"images": images, "models": {p.name: file_hash(p) for p in sorted(model.glob("*.gguf"))},
            "configuration": {"context": args.context, "threads": args.threads,
                              "cold_runs": args.cold_runs, "warm_runs": args.warm_runs,
                               "variant":args.variant,"memory":args.memory},
            "workloads_sha256": file_hash(args.workloads), "runs": []}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    for cold in range(args.cold_runs):
        order = ["reference", "candidate"] if cold % 2 == 0 else ["candidate", "reference"]
        for backend in order:
            print(f"{backend}: cold run {cold + 1}/{args.cold_runs}", flush=True)
            try:
                run = run_image(images[backend]["Id"], model, workloads, backend,
                                args.warm_runs+1 if cold==0 else 1, args.context, args.threads,
                                 args.variant,args.memory)
            except (RuntimeError, OSError, subprocess.SubprocessError) as error:
                run = {"backend":backend,"startup_error":str(error),"results":[
                    {"workload":workload["name"],"passed":False,"error":str(error)} for workload in workloads]}
            data["runs"].append(run)
            args.output.write_text(json.dumps(data, indent=2) + "\n")
            args.output.with_suffix(".md").write_text(report(data))


if __name__ == "__main__":
    main()
