import base64
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import numpy as np

from tts_vits import runtime


class FakeAdapter:
    sample_rate = 16_000

    def synthesize(self, _text):
        return np.linspace(-1.5, 1.5, 3200, dtype=np.float32)


class RuntimeTest(unittest.TestCase):
    def test_single_file_layout(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            for filename in ("config.json", "vocab.json", "model.safetensors"):
                (path / filename).write_text("{}", encoding="utf-8")
            runtime.validate_model_layout(path)

    def test_sharded_layout_requires_every_shard(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            (path / "config.json").write_text("{}", encoding="utf-8")
            (path / "vocab.json").write_text("{}", encoding="utf-8")
            (path / "model.safetensors.index.json").write_text(
                json.dumps({"weight_map": {"a": "model-00001-of-00002.safetensors"}}),
                encoding="utf-8",
            )
            with self.assertRaises(runtime.RuntimeErrorCode):
                runtime.validate_model_layout(path)

    def test_request_rejects_voice_and_unsupported_language(self):
        with self.assertRaises(runtime.RuntimeErrorCode) as raised:
            runtime.validate_request({"type": "synthesize", "text": "hello", "voice_id": "speaker"})
        self.assertEqual(raised.exception.code, "invalid_input")

        with mock.patch.object(runtime, "SUPPORTED_LANGUAGES", frozenset({"eng"})):
            with self.assertRaises(runtime.RuntimeErrorCode) as raised:
                runtime.validate_request({"type": "synthesize", "text": "hello", "language_hint": "de"})
        self.assertEqual(raised.exception.code, "unsupported_language")

    def test_request_accepts_iso_639_and_bcp47_aliases(self):
        with mock.patch.object(runtime, "SUPPORTED_LANGUAGES", frozenset({"eng"})):
            _, language = runtime.validate_request(
                {"type": "synthesize", "text": "hello", "language_hint": "en-US"}
            )

        self.assertEqual(language, "eng")

    def test_waveform_is_clipped_to_little_endian_s16(self):
        pcm = runtime.waveform_to_pcm(np.asarray([-2.0, -0.5, 0.5, 2.0, 0.0]))
        values = np.frombuffer(pcm, dtype="<i2")
        np.testing.assert_array_equal(values, [-32767, -16384, 16384, 32767, 0])

    def test_non_finite_waveform_is_rejected(self):
        with self.assertRaises(runtime.RuntimeErrorCode) as raised:
            runtime.waveform_to_pcm(np.asarray([0.0, np.nan]))
        self.assertEqual(raised.exception.code, "invalid_input")

    def test_synthesis_emits_frame_aligned_chunks_and_terminal_event(self):
        events = runtime.handle_request(
            {"id": "req-1", "type": "synthesize", "text": "Hello.", "voice_id": ""},
            FakeAdapter(),
        )
        chunks = events[:-1]
        self.assertGreater(len(chunks), 1)
        self.assertEqual((chunks[0]["sample_rate"], chunks[0]["channels"], chunks[0]["sample_format"]), (16000, 1, "s16le"))
        self.assertTrue(all(len(base64.b64decode(chunk["audio"])) % 2 == 0 for chunk in chunks))
        self.assertEqual(events[-1], {"id": "req-1", "audio": "", "done": True})


if __name__ == "__main__":
    unittest.main()
