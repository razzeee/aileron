"""Probe mixed operations on one server. The socket mount is test-only."""
import argparse
import http.client
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import uuid


class UnixHTTP(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("localhost", timeout=120)
        self.path = str(path)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


def request(path, route, body=None):
    connection = UnixHTTP(path)
    try:
        connection.request("GET" if body is None else "POST", route,
                           None if body is None else json.dumps(body),
                           {"Content-Type": "application/json"})
        response = connection.getresponse()
        return {"status": response.status, "body": json.loads(response.read())}
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--server-binary", default="/usr/local/bin/llama-server")
    parser.add_argument("--resource-policy", action="store_true")
    parser.add_argument("--tools", action="store_true")
    parser.add_argument("--props-only", action="store_true", help="inspect template capabilities without inference")
    parser.add_argument("--memory", default="8g")
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    name = "aileron-probe-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(dir=args.output.parent.resolve()) as directory:
        path = Path(directory) / "server.sock"
        command = ["podman", "run", "--rm", "--name", name, "--network", "none",
                   "--read-only", "--security-opt=label=disable", "--cap-drop=ALL", "--tmpfs", "/tmp",
                   f"--memory={args.memory}", "--pids-limit=256",
                   "--mount", f"type=bind,source={args.model_dir.resolve()},destination=/model,ro=true",
                   "--mount", f"type=bind,source={directory},destination=/probe",
                   "--entrypoint", args.server_binary, args.image,
                   "--model", "/model/model.gguf", "--host", "/probe/server.sock",
                   "--no-webui", "--jinja", "--fit", "off", "--no-warmup", "--parallel", "1", "--threads", "2", "--threads-http", "2",
                   "--ctx-size", "4096", "--embeddings", "--pooling", "mean"]
        with args.output.with_suffix(".log").open("w") as log:
            process = subprocess.Popen(command, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 120
                while True:
                    try:
                        if request(path, "/health")["status"] == 200:
                            break
                    except (OSError, ValueError):
                        pass
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError("server did not become healthy; inspect probe log")
                    time.sleep(0.05)
                chat = {"messages":[{"role":"user","content":"Say hello briefly."}],
                        "max_tokens":16,"temperature":0,"stream":False}
                requests = [("/apply-template", chat, 200), ("/v1/chat/completions", chat, 200),
                            ("/embeddings", {"input":"A cat sits on a mat.", "embd_normalize":-1}, 200),
                            ("/v1/chat/completions", chat, 200)]
                if args.resource_policy and not args.props_only:
                    requests += [("/v1/chat/completions", {**chat, "max_tokens":4080,
                                  "aileron_reserve_output":True}, 400),
                                 ("/v1/chat/completions", {**chat, "max_tokens":32, "ignore_eos":True,
                                  "cache_prompt":False,"aileron_decode_delay_ms":0}, 200),
                                 ("/v1/chat/completions", {**chat, "max_tokens":32, "ignore_eos":True,
                                  "cache_prompt":False,"aileron_decode_delay_ms":20}, 200)]
                if args.tools:
                    fixture=json.loads(Path(__file__).with_name("tools.json").read_text())[0]["request"]
                    requests.append(("/v1/chat/completions",{
                        "messages":[{"role":"system","content":fixture["system"]},{"role":"user","content":fixture["prompt"]}],
                        "tools":[{"type":"function","function":{"name":tool["name"],"description":tool["description"],"parameters":json.loads(tool["schema_json"])}} for tool in fixture["tools"]],
                        "max_tokens":256,"temperature":0.0,"stream":False,"reasoning_format":"deepseek",
                        "chat_template_kwargs":{"enable_thinking":False},
                    },200))
                    requests.insert(-1,("/apply-template",requests[-1][1],200))
                results = [{"route":"/props", "response":request(path,"/props")}]
                for route, body, expected in ([] if args.props_only else requests):
                    started = time.monotonic()
                    results.append({"route":route, "request":body, "response":request(path, route, body),
                                    "seconds":time.monotonic() - started})
                    if results[-1]["response"]["status"] != expected:
                        raise RuntimeError(f"unexpected response: {results[-1]}")
                args.output.write_text(json.dumps(results, indent=2) + "\n")
                if args.resource_policy and not args.props_only:
                    background=next(item for item in results if item.get("request",{}).get("aileron_decode_delay_ms")==20)
                    interactive=next(item for item in results if item.get("request",{}).get("aileron_decode_delay_ms")==0)
                    difference = (background["response"]["body"]["timings"]["predicted_ms"]
                                  - interactive["response"]["body"]["timings"]["predicted_ms"])
                    if difference < 400:
                        raise RuntimeError(f"background decode pacing added only {difference} ms")
                print([(result["route"], result["response"]["status"]) for result in results])
            finally:
                subprocess.run(["podman", "rm", "--force", "--ignore", name], capture_output=True)
                process.wait(timeout=10)


if __name__ == "__main__":
    main()
