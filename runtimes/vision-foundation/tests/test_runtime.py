import base64
import io
import json
import unittest
import unittest.mock

import numpy as np
from PIL import Image

from vision_foundation import runtime


def tiny_png_base64() -> str:
    image = Image.new("RGB", (2, 2), (255, 0, 0))
    out = io.BytesIO()
    image.save(out, format="PNG")
    return base64.b64encode(out.getvalue()).decode("ascii")


class RuntimeHelpersTest(unittest.TestCase):
    def test_decode_image_accepts_base64_png(self):
        decoded = runtime.decode_image(tiny_png_base64())

        self.assertEqual(decoded.width, 2)
        self.assertEqual(decoded.height, 2)

    def test_point_prompts_convert_normalized_coordinates_to_pixels(self):
        coords, labels = runtime.point_prompts([{"x": 0.25, "y": 0.5, "positive": False}], 200, 100)

        np.testing.assert_array_equal(coords, np.asarray([[50.0, 50.0]], dtype=np.float32))
        np.testing.assert_array_equal(labels, np.asarray([0], dtype=np.int32))

    def test_box_prompts_reject_out_of_range_boxes(self):
        with self.assertRaises(runtime.RuntimeErrorCode) as raised:
            runtime.box_prompts([{"x": 0.8, "y": 0.1, "width": 0.3, "height": 0.2}], 10, 10)

        self.assertEqual(raised.exception.code, "invalid_input")

    def test_encode_mask_png_returns_png_bytes(self):
        encoded = runtime.encode_mask_png(np.asarray([[True, False], [False, True]]))

        raw = base64.b64decode(encoded)
        self.assertTrue(raw.startswith(b"\x89PNG\r\n\x1a\n"))

    def test_normalize_depth_preserves_raw_range(self):
        values, minimum, maximum = runtime.normalize_depth(np.asarray([[2.0, 4.0], [6.0, 10.0]], dtype=np.float32))

        self.assertEqual(minimum, 2.0)
        self.assertEqual(maximum, 10.0)
        self.assertEqual(values, [0.0, 0.25, 0.5, 1.0])

    def test_result_response_wraps_result_as_json_string(self):
        response = runtime.result_response("req-1", {"detections": []})

        self.assertEqual(response["id"], "req-1")
        self.assertTrue(response["done"])
        self.assertEqual(json.loads(response["result"]), {"detections": []})

    def test_depth_model_path_accepts_flat_installed_artifacts(self):
        with unittest.mock.patch.object(runtime, "MODEL_DIR", self.create_temp_depth_dir()):
            self.assertIsNotNone(runtime.depth_model_path())

    def test_da3_model_detection_reads_config(self):
        path = self.create_temp_depth_dir()
        (path / "config.json").write_text('{"model_type":"depth-anything-3"}', encoding="utf-8")

        self.assertTrue(runtime.is_da3_model(path))

    def test_unknown_request_uses_stable_error(self):
        with self.assertRaises(runtime.RuntimeErrorCode) as raised:
            runtime.handle_request({"id": "req-1", "type": "classify"})

        self.assertEqual(raised.exception.code, "unsupported_request")

    def test_segment_rejects_multiple_box_prompts_before_loading_model(self):
        with self.assertRaises(runtime.RuntimeErrorCode) as raised:
            runtime.handle_segment(
                {
                    "id": "req-1",
                    "image": tiny_png_base64(),
                    "type": "segment",
                    "boxes": [
                        {"x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5},
                        {"x": 0.5, "y": 0.5, "width": 0.5, "height": 0.5},
                    ],
                }
            )

        self.assertEqual(raised.exception.code, "invalid_input")

    def create_temp_depth_dir(self):
        import tempfile

        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        path = runtime.Path(temp.name)
        for filename in ("config.json", "model.safetensors", "preprocessor_config.json"):
            (path / filename).write_bytes(b"stub")
        return path


if __name__ == "__main__":
    unittest.main()
