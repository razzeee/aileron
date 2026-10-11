"""Exercise dispatch without loading models or replacing installed binaries."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ENTRYPOINT = Path(__file__).resolve().parents[1] / "entrypoint.sh"


class EntrypointTests(unittest.TestCase):
    def dispatch(self, model, projector=False):
        with tempfile.TemporaryDirectory() as directory:
            mmproj = Path(directory) / "mmproj.gguf"
            if projector:
                mmproj.touch()
            env = {
                **os.environ,
                "MODEL_PATH": model,
                "MMPROJ_PATH": str(mmproj),
            }
            # Intercept exec in the shell so the actual entrypoint selects the binary.
            return subprocess.run(
                ["bash", "-c", 'exec() { printf "%s\\n" "$@"; exit 0; }; source "$1"',
                 "test-entrypoint", str(ENTRYPOINT)],
                env=env, capture_output=True, text=True, check=False,
            )

    def test_text_selection(self):
        result = self.dispatch("/model/model.gguf")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/usr/local/bin/aileron-runtime-llm-llama-server")

    def test_whisper_dispatch(self):
        result = self.dispatch("/model/model.bin")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/usr/local/bin/aileron-runtime-asr-whisper-cpp")

    def test_vision_dispatch(self):
        result = self.dispatch("/model/model.gguf", projector=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "/usr/local/bin/aileron-runtime-llm-llama-server")


if __name__ == "__main__":
    unittest.main()
