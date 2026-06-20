import importlib.util
import pathlib
import unittest
from unittest import mock


MODULE_PATH = pathlib.Path(__file__).with_name("entrypoint.py")
SPEC = importlib.util.spec_from_file_location("llamafile_entrypoint", MODULE_PATH)
entrypoint = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(entrypoint)


class FakeRuntime:
    def __init__(self, responses):
        self.responses = list(responses)
        self.requests = []

    def request_json(self, path, payload, **_kwargs):
        self.requests.append((path, payload))
        return self.responses.pop(0)


class FakeStreamingResponse:
    def __init__(self, lines):
        self.lines = iter(lines)
        self.read_count = 0

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def __iter__(self):
        return self

    def __next__(self):
        self.read_count += 1
        return next(self.lines)


class LlamafileEntrypointTests(unittest.TestCase):
    def test_chat_payload_uses_openai_messages(self):
        payload = entrypoint.chat_payload(
            {"prompt": "Hello", "system": "System", "max_tokens": 12},
            stream=True,
        )

        self.assertEqual(payload["max_tokens"], 12)
        self.assertTrue(payload["stream"])
        self.assertEqual(
            payload["messages"],
            [
                {"role": "system", "content": "System"},
                {"role": "user", "content": "Hello"},
            ],
        )

    def test_completion_payload_keeps_predict_next_raw(self):
        payload = entrypoint.completion_payload(
            {"prompt": "The lighthouse", "max_tokens": 4, "temperature": 0.2}
        )

        self.assertEqual(payload["prompt"], "The lighthouse")
        self.assertEqual(payload["n_predict"], 4)
        self.assertEqual(payload["temperature"], 0.2)
        self.assertFalse(payload["stream"])
        self.assertNotIn("messages", payload)

    def test_structured_payload_sends_json_schema_constraint(self):
        schema = {"type": "object", "required": ["name"], "properties": {"name": {"type": "string"}}}
        payload = entrypoint.structured_payload(
            {
                "prompt": "Extract",
                "system": "Return JSON",
                "max_tokens": 64,
                "response_format": {"type": "json_schema", "schema": schema},
            }
        )

        self.assertEqual(payload["json_schema"], schema)
        self.assertEqual(payload["n_predict"], 64)
        self.assertIn("Return JSON", payload["prompt"])
        self.assertIn("Extract", payload["prompt"])

    def test_validated_structured_result_rejects_schema_mismatch(self):
        runtime = FakeRuntime([{"content": '{"age": 36}'}])
        req = {
            "response_format": {
                "schema": {
                    "type": "object",
                    "required": ["name"],
                    "properties": {"name": {"type": "string"}},
                }
            }
        }

        with self.assertRaises(entrypoint.ValidationError):
            entrypoint.validated_structured_result(runtime, req)

    def test_normalize_embedding_mean_pools_token_vectors(self):
        response = {"embedding": [[1, 3, 5], [3, 5, 7]]}

        self.assertEqual(entrypoint.normalize_embedding(response), [2.0, 4.0, 6.0])

    def test_accelerator_without_offload_is_rejected(self):
        old_device = entrypoint.AILERON_DEVICE
        old_layers = entrypoint.os.environ.get("N_GPU_LAYERS")
        try:
            entrypoint.AILERON_DEVICE = "cuda"
            entrypoint.os.environ["N_GPU_LAYERS"] = "0"
            with self.assertRaises(RuntimeError):
                entrypoint.llamafile_command(8080)
        finally:
            entrypoint.AILERON_DEVICE = old_device
            if old_layers is None:
                entrypoint.os.environ.pop("N_GPU_LAYERS", None)
            else:
                entrypoint.os.environ["N_GPU_LAYERS"] = old_layers

    def test_accelerator_command_requests_explicit_gpu_backend(self):
        old_device = entrypoint.AILERON_DEVICE
        old_layers = entrypoint.os.environ.get("N_GPU_LAYERS")
        old_kind = entrypoint.LLAMAFILE_SERVER_KIND
        try:
            entrypoint.AILERON_DEVICE = "vulkan"
            entrypoint.LLAMAFILE_SERVER_KIND = "llamafile"
            entrypoint.os.environ.pop("N_GPU_LAYERS", None)

            command = entrypoint.llamafile_command(8080)
        finally:
            entrypoint.AILERON_DEVICE = old_device
            entrypoint.LLAMAFILE_SERVER_KIND = old_kind
            if old_layers is None:
                entrypoint.os.environ.pop("N_GPU_LAYERS", None)
            else:
                entrypoint.os.environ["N_GPU_LAYERS"] = old_layers

        self.assertIn("--gpu", command)
        self.assertIn("amd", command)
        self.assertIn("-ngl", command)
        self.assertIn("-1", command)

    def test_native_llama_server_command_uses_native_flags(self):
        old_device = entrypoint.AILERON_DEVICE
        old_kind = entrypoint.LLAMAFILE_SERVER_KIND
        old_runner = entrypoint.LLAMAFILE_RUNNER
        old_path = entrypoint.LLAMAFILE_PATH
        old_layers = entrypoint.os.environ.get("N_GPU_LAYERS")
        try:
            entrypoint.AILERON_DEVICE = "vulkan"
            entrypoint.LLAMAFILE_SERVER_KIND = "llama-server"
            entrypoint.LLAMAFILE_RUNNER = ""
            entrypoint.LLAMAFILE_PATH = "/usr/local/bin/llama-server"
            entrypoint.os.environ.pop("N_GPU_LAYERS", None)

            command = entrypoint.llamafile_command(8080)
        finally:
            entrypoint.AILERON_DEVICE = old_device
            entrypoint.LLAMAFILE_SERVER_KIND = old_kind
            entrypoint.LLAMAFILE_RUNNER = old_runner
            entrypoint.LLAMAFILE_PATH = old_path
            if old_layers is None:
                entrypoint.os.environ.pop("N_GPU_LAYERS", None)
            else:
                entrypoint.os.environ["N_GPU_LAYERS"] = old_layers

        self.assertEqual(command[0], "/usr/local/bin/llama-server")
        self.assertIn("--n-gpu-layers", command)
        self.assertIn("-1", command)
        self.assertNotIn("--gpu", command)
        self.assertNotIn("--server", command)

    def test_stream_json_yields_before_reading_full_response(self):
        response = FakeStreamingResponse(
            [
                b'data: {"content":"first"}\n',
                b'data: {"content":"second"}\n',
                b'data: [DONE]\n',
            ]
        )
        runtime = entrypoint.LlamafileRuntime("http://127.0.0.1:8080")

        with mock.patch.object(entrypoint, "urlopen", return_value=response):
            stream = runtime.stream_json("/completion", {"prompt": "hi"})
            self.assertEqual(next(stream), {"content": "first"})

        self.assertEqual(response.read_count, 1)


if __name__ == "__main__":
    unittest.main()
