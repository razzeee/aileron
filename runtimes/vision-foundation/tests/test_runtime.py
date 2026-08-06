import base64
import io
import json
import sys
import types
import unittest
import unittest.mock
from pathlib import Path

import numpy as np
from PIL import Image

from vision_foundation import runtime


def tiny_png_base64() -> str:
    return png_base64(2, 2)


def png_base64(width: int, height: int) -> str:
    image = Image.new("RGB", (width, height), (255, 0, 0))
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

    def test_prepare_depth_response_downsamples_large_maps(self):
        response = runtime.prepare_depth_response(
            np.arange(100, dtype=np.float32).reshape(10, 10),
            max_pixels=16,
        )

        self.assertEqual(response["width"], 4)
        self.assertEqual(response["height"], 4)
        self.assertEqual(len(response["values"]), 16)
        self.assertEqual(response["unit"], "meter")
        self.assertEqual(response["minimum"], 0.0)
        self.assertEqual(response["maximum"], 99.0)
        self.assertEqual(response["values"][-1], 99.0)

    def test_prepare_depth_response_rejects_negative_values(self):
        with self.assertRaises(runtime.RuntimeErrorCode) as raised:
            runtime.prepare_depth_response(np.asarray([[1.0, -0.1]], dtype=np.float32))

        self.assertEqual(raised.exception.code, "inference_failed")

    def test_result_response_wraps_result_as_json_string(self):
        response = runtime.result_response("req-1", {"detections": []})

        self.assertEqual(response["id"], "req-1")
        self.assertTrue(response["done"])
        self.assertEqual(json.loads(response["result"]), {"detections": []})

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

    def test_detect_redirects_model_stdout_away_from_protocol(self):
        class FakeYolo:
            names = {}

            def __init__(self, _path):
                print("noisy detector init")

            def predict(self, _image, verbose=False):
                print("noisy detector predict")
                return []

        ultralytics = types.ModuleType("ultralytics")
        ultralytics.YOLO = FakeYolo
        path = self.create_temp_model_dir(("model.pt",))
        stdout = io.StringIO()
        stderr = io.StringIO()
        with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
            with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                with unittest.mock.patch("sys.stdout", stdout), unittest.mock.patch("sys.stderr", stderr):
                    response = runtime.handle_detect({"id": "req-1", "type": "detect", "image": tiny_png_base64()})

        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("noisy detector init", stderr.getvalue())
        self.assertEqual(json.loads(response["result"]), {"detections": []})

    def test_segment_passes_pixel_prompts_and_resizes_mask(self):
        calls = []

        class FakeSam:
            def __init__(self, path):
                assert Path(path).name == "sam2.1_t.pt"
                assert Path(path).name != "model.pt"
                self.is_sam2 = False
                calls.append(("init", path))

            def predict(self, image, **kwargs):
                calls.append(("predict", image.size, kwargs, self.is_sam2))
                return [types.SimpleNamespace(
                    masks=types.SimpleNamespace(data=np.asarray([[[0.0, 1.0], [0.0, 0.0]]])),
                    boxes=types.SimpleNamespace(conf=np.asarray([0.75])),
                )]

        ultralytics = types.ModuleType("ultralytics")
        ultralytics.SAM = FakeSam
        path = self.create_temp_model_dir(("model.pt",))
        with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
            with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                response = runtime.handle_segment(
                    {
                        "id": "req-1",
                        "type": "segment",
                        "image": png_base64(4, 2),
                        "points": [
                            {"x": 0.25, "y": 0.5, "positive": True},
                            {"x": 0.75, "y": 0.5, "positive": False},
                        ],
                        "boxes": [{"x": 0.25, "y": 0.0, "width": 0.5, "height": 1.0}],
                    }
                )

        self.assertEqual(Path(calls[0][1]).name, "sam2.1_t.pt")
        self.assertNotEqual(Path(calls[0][1]).name, "model.pt")
        self.assertEqual(calls[1][1], (4, 2))
        self.assertEqual(calls[1][2]["points"], [[[1.0, 1.0], [3.0, 1.0]]])
        self.assertEqual(calls[1][2]["labels"], [[1, 0]])
        self.assertEqual(calls[1][2]["bboxes"], [1.0, 0.0, 3.0, 2.0])
        self.assertEqual(calls[1][2]["conf"], 0.0)
        self.assertTrue(calls[1][3])
        masks = json.loads(response["result"])["masks"]
        self.assertEqual(len(masks), 1)
        self.assertEqual(masks[0]["confidence"], 0.75)
        self.assertEqual(masks[0]["mask_width"], 2)
        self.assertEqual(masks[0]["mask_height"], 1)
        self.assertEqual(masks[0]["x"], 0.5)
        self.assertEqual(masks[0]["y"], 0.0)
        self.assertEqual(masks[0]["width"], 0.5)
        self.assertEqual(masks[0]["height"], 0.5)

    def test_segment_rejects_missing_or_inconsistent_scores(self):
        for scores in (None, np.asarray([0.5, 0.6]), np.asarray([np.nan])):
            class FakeSam:
                def __init__(self, _path):
                    pass

                def predict(self, _image, **_kwargs):
                    boxes = None if scores is None else types.SimpleNamespace(conf=scores)
                    return [types.SimpleNamespace(
                        masks=types.SimpleNamespace(data=np.ones((1, 2, 2))),
                        boxes=boxes,
                    )]

            ultralytics = types.ModuleType("ultralytics")
            ultralytics.SAM = FakeSam
            path = self.create_temp_model_dir(("model.pt",))
            with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
                with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                    with self.assertRaises(runtime.RuntimeErrorCode) as raised:
                        runtime.handle_segment({
                            "id": "req-1",
                            "type": "segment",
                            "image": tiny_png_base64(),
                            "points": [{"x": 0.5, "y": 0.5}],
                        })
            self.assertEqual(raised.exception.code, "inference_failed")

    def test_depth_reads_ultralytics_depth_data_and_preserves_meters(self):
        calls = []

        class FakeYolo:
            def __init__(self, path):
                calls.append(("init", path))

            def predict(self, image, verbose=False):
                calls.append(("predict", image.size, verbose))
                return [types.SimpleNamespace(
                    depth=types.SimpleNamespace(data=np.asarray([[0.5, 2.5], [4.0, 8.0]]))
                )]

        ultralytics = types.ModuleType("ultralytics")
        ultralytics.YOLO = FakeYolo
        path = self.create_temp_model_dir(("model.pt",))
        with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
            with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                response = runtime.handle_depth({
                    "id": "req-1",
                    "type": "depth",
                    "image": tiny_png_base64(),
                })

        self.assertEqual(calls[0], ("init", str(path / "model.pt")))
        self.assertEqual(calls[1], ("predict", (2, 2), False))
        depth = json.loads(response["result"])["depth"]
        self.assertEqual(depth["width"], 2)
        self.assertEqual(depth["height"], 2)
        self.assertEqual(depth["values"], [0.5, 2.5, 4.0, 8.0])
        self.assertEqual(depth["unit"], "meter")
        self.assertEqual((depth["minimum"], depth["maximum"]), (0.5, 8.0))

    def create_temp_model_dir(self, filenames):
        import tempfile

        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        path = runtime.Path(temp.name)
        for filename in filenames:
            (path / filename).write_bytes(b"stub")
        return path


if __name__ == "__main__":
    unittest.main()
