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
    def setUp(self):
        runtime._model_cache.clear()
        self.addCleanup(runtime._model_cache.clear)

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
                alias = Path(path)
                resolved = alias.resolve()
                assert alias.name == "sam2.1_t.pt"
                assert alias.name != "model.pt"
                assert alias.exists()
                assert resolved.name == "model.pt"
                assert resolved.is_file()
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

    def test_yolo_reuses_model_for_sequential_requests(self):
        for request_type in ("detect", "depth"):
            with self.subTest(request_type=request_type):
                loads = []
                images = []

                class FakeYolo:
                    names = {}

                    def __init__(self, path):
                        loads.append(path)

                    def predict(self, image, verbose=False):
                        images.append(image.size)
                        return [types.SimpleNamespace(
                            boxes=None,
                            depth=types.SimpleNamespace(data=np.ones((image.height, image.width))),
                        )]

                ultralytics = types.ModuleType("ultralytics")
                ultralytics.YOLO = FakeYolo
                path = self.create_temp_model_dir(("model.pt",))
                with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
                    with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                        responses = [runtime.handle_request({
                            "id": str(width), "type": request_type, "image": png_base64(width, 2),
                        }) for width in (2, 4)]
                self.assertEqual(len(loads), 1)
                self.assertEqual(images, [(2, 2), (4, 2)])
                self.assertEqual([response["id"] for response in responses], ["2", "4"])

    def test_sam_reuses_model_and_keeps_checkpoint_alias_alive(self):
        loads = []
        calls = []

        class FakeSam:
            def __init__(self, path):
                self.path = Path(path)
                loads.append(self.path)

            def predict(self, image, **kwargs):
                self_path_exists = self.path.is_file()
                calls.append((image.size, kwargs, self_path_exists))
                return [types.SimpleNamespace(
                    masks=types.SimpleNamespace(data=np.ones((1, image.height, image.width))),
                    boxes=types.SimpleNamespace(conf=np.asarray([0.75])),
                )]

        ultralytics = types.ModuleType("ultralytics")
        ultralytics.SAM = FakeSam
        path = self.create_temp_model_dir(("model.pt",))
        with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
            with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                runtime.handle_request({
                    "id": "points", "type": "segment", "image": tiny_png_base64(),
                    "points": [{"x": 0.5, "y": 0.5}],
                })
                self.assertTrue(loads[0].is_file())
                runtime.handle_request({
                    "id": "box", "type": "segment", "image": png_base64(4, 2),
                    "boxes": [{"x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5}],
                })
        self.assertEqual(len(loads), 1)
        self.assertEqual([call[0] for call in calls], [(2, 2), (4, 2)])
        self.assertTrue(all(call[2] for call in calls))
        self.assertIsNone(calls[0][1]["bboxes"])
        self.assertIsNone(calls[1][1]["points"])
        self.assertIsNone(calls[1][1]["labels"])
        runtime._model_cache.clear()
        self.assertFalse(loads[0].exists())

    def test_sam_clears_image_and_prompts_after_success_and_failure(self):
        predictors = []

        class FakePredictor:
            def __init__(self):
                self.im = None
                self.features = None
                self.prompts = {}
                self.segment_all = False

            def reset_image(self):
                self.im = None
                self.features = None

            def set_prompts(self, prompts):
                self.prompts = prompts

        class FakeSam:
            def __init__(self, _path):
                self.predictor = None

            def predict(self, image, **kwargs):
                if self.predictor is None:
                    self.predictor = FakePredictor()
                    predictors.append(self.predictor)
                predictor = self.predictor
                assert predictor.im is None and predictor.features is None
                assert predictor.prompts == {} and not predictor.segment_all
                predictor.im = image
                predictor.features = "old image features"
                predictor.prompts = kwargs
                predictor.segment_all = True
                if image.width == 4:
                    raise ValueError("inference failed after setting state")
                return [types.SimpleNamespace(
                    masks=types.SimpleNamespace(data=np.ones((1, 2, 2))),
                    boxes=types.SimpleNamespace(conf=np.asarray([0.75])),
                )]

        ultralytics = types.ModuleType("ultralytics")
        ultralytics.SAM = FakeSam
        path = self.create_temp_model_dir(("model.pt",))
        with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
            with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                for width in (2, 2, 4, 2):
                    request = {
                        "id": "sam", "type": "segment", "image": png_base64(width, 2),
                        "points": [{"x": 0.5, "y": 0.5}],
                    }
                    if width == 4:
                        with self.assertRaises(runtime.RuntimeErrorCode):
                            runtime.handle_request(request)
                    else:
                        runtime.handle_request(request)
                    self.assertIsNone(predictors[-1].im)
                    self.assertIsNone(predictors[-1].features)
                    self.assertEqual(predictors[-1].prompts, {})
                    self.assertFalse(predictors[-1].segment_all)
        self.assertEqual(len(predictors), 2)

    def test_yolo_retries_failed_load_and_prediction(self):
        for failure_stage in ("load", "predict"):
            with self.subTest(failure_stage=failure_stage):
                loads = []

                class FakeYolo:
                    names = {}

                    def __init__(self, path):
                        loads.append(path)
                        if len(loads) == 1 and failure_stage == "load":
                            raise ValueError("failed load")

                    def predict(self, _image, **_kwargs):
                        if len(loads) == 1:
                            raise ValueError("failed prediction")
                        return []

                ultralytics = types.ModuleType("ultralytics")
                ultralytics.YOLO = FakeYolo
                path = self.create_temp_model_dir(("model.pt",))
                with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
                    with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                        request = {"id": "retry", "type": "detect", "image": tiny_png_base64()}
                        with self.assertRaises(runtime.RuntimeErrorCode) as raised:
                            runtime.handle_request(request)
                        self.assertEqual(raised.exception.code, "inference_failed")
                        self.assertTrue(runtime.handle_request(request)["done"])
                        self.assertTrue(runtime.handle_request(request)["done"])
                self.assertEqual(len(loads), 2)

    def test_cache_replaces_model_when_checkpoint_or_loader_changes(self):
        sam = unittest.mock.Mock(return_value=types.SimpleNamespace())
        yolo = unittest.mock.Mock(return_value=types.SimpleNamespace())
        first = self.create_temp_model_dir(("model.pt",)) / "model.pt"
        second = self.create_temp_model_dir(("model.pt",)) / "model.pt"
        runtime._model_cache.get("sam", first, sam)
        alias = Path(sam.call_args.args[0])
        runtime._model_cache.get("yolo", first, yolo)
        self.assertFalse(alias.exists())
        runtime._model_cache.get("yolo", second, yolo)
        self.assertEqual(yolo.call_count, 2)

    def test_result_processing_failures_evict_model_and_preserve_errors(self):
        cases = [
            ("detect", [types.SimpleNamespace(boxes=1)], "YOLO inference failed:"),
            ("segment", [], "SAM returned an unexpected result count"),
            ("segment", [types.SimpleNamespace(masks=types.SimpleNamespace(data=[[["bad"]]]),
                                             boxes=types.SimpleNamespace(conf=[0.75]))], "SAM inference failed:"),
            ("depth", [], "YOLO depth returned an unexpected result count"),
            ("depth", [types.SimpleNamespace(depth=types.SimpleNamespace(data=[[float("nan")]]))],
             "depth output must be finite, nonnegative and non-empty"),
            ("depth", [types.SimpleNamespace(depth=types.SimpleNamespace(data=[["bad"]]))],
             "YOLO depth inference failed:"),
        ]
        for task, malformed_results, expected_reason in cases:
            with self.subTest(task=task, expected_reason=expected_reason):
                runtime._model_cache.clear()
                loads = []

                class FakeModel:
                    names = {}

                    def __init__(self, path):
                        loads.append(Path(path))
                        self.malformed = len(loads) == 1

                    def predict(self, _image, **_kwargs):
                        if self.malformed:
                            return malformed_results
                        return [types.SimpleNamespace(
                            boxes=types.SimpleNamespace(conf=np.asarray([0.75])) if task == "segment" else [],
                            masks=types.SimpleNamespace(data=np.ones((1, 2, 2))),
                            depth=types.SimpleNamespace(data=np.ones((2, 2))),
                        )]

                ultralytics = types.ModuleType("ultralytics")
                ultralytics.YOLO = ultralytics.SAM = FakeModel
                path = self.create_temp_model_dir(("model.pt",))
                request = {"id": "retry", "type": task, "image": tiny_png_base64(),
                           "points": [{"x": 0.5, "y": 0.5}]}
                with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
                    with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                        with self.assertRaises(runtime.RuntimeErrorCode) as raised:
                            runtime.handle_request(request)
                        self.assertEqual(raised.exception.code, "inference_failed")
                        if expected_reason.endswith(":"):
                            self.assertTrue(raised.exception.reason.startswith(expected_reason))
                        else:
                            self.assertEqual(raised.exception.reason, expected_reason)
                        if task == "segment":
                            self.assertFalse(loads[0].exists())
                        self.assertTrue(runtime.handle_request(request)["done"])
                        self.assertTrue(runtime.handle_request(request)["done"])
                self.assertEqual(len(loads), 2)

    def test_failed_sam_load_removes_alias_and_can_retry(self):
        aliases = []

        def load(path):
            aliases.append(Path(path))
            if len(aliases) == 1:
                raise ValueError("failed checkpoint load")
            return types.SimpleNamespace()

        path = self.create_temp_model_dir(("model.pt",)) / "model.pt"
        with self.assertRaises(ValueError):
            runtime._model_cache.get("sam", path, load)
        self.assertFalse(aliases[0].exists())
        runtime._model_cache.get("sam", path, load)
        self.assertTrue(aliases[1].is_file())

    def test_protocol_loop_reuses_model_and_cleans_up_at_eof(self):
        loads = []

        class FakeSam:
            def __init__(self, path):
                print("noisy SAM init")
                loads.append(Path(path))

            def predict(self, image, **_kwargs):
                print("noisy SAM prediction")
                return [types.SimpleNamespace(
                    masks=types.SimpleNamespace(data=np.ones((1, image.height, image.width))),
                    boxes=types.SimpleNamespace(conf=np.asarray([0.75])),
                )]

        ultralytics = types.ModuleType("ultralytics")
        ultralytics.SAM = FakeSam
        requests = [{
            "id": str(index), "type": "segment", "image": tiny_png_base64(),
            "points": [{"x": 0.5, "y": 0.5}],
        } for index in range(2)]
        stdin = io.StringIO("\n".join(json.dumps(request) for request in requests))
        stdout, stderr = io.StringIO(), io.StringIO()
        path = self.create_temp_model_dir(("model.pt",))
        with unittest.mock.patch.dict(sys.modules, {"ultralytics": ultralytics}):
            with unittest.mock.patch.object(runtime, "MODEL_DIR", path):
                with unittest.mock.patch("sys.stdin", stdin), unittest.mock.patch("sys.stdout", stdout), unittest.mock.patch("sys.stderr", stderr):
                    self.assertEqual(runtime.main(), 0)
        responses = [json.loads(line) for line in stdout.getvalue().splitlines()]
        self.assertEqual([response["id"] for response in responses], ["0", "1"])
        self.assertTrue(all(response["done"] and "result" in response for response in responses))
        self.assertEqual(len(loads), 1)
        self.assertFalse(loads[0].exists())
        self.assertIn("noisy SAM", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
