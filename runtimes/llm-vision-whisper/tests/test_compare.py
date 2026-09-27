import importlib.util
from pathlib import Path
import sys
import unittest


SPEC = importlib.util.spec_from_file_location("compare", Path(__file__).resolve().parents[1] / "bench/compare.py")
compare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(compare)


class CompareTests(unittest.TestCase):
    def test_embedding_comparison_checks_dimensions_and_direction(self):
        self.assertAlmostEqual(compare.cosine([1, 2], [2, 4]), 1.0)
        self.assertAlmostEqual(compare.cosine([1, 0], [0, 1]), 0.0)
        with self.assertRaises(ValueError):
            compare.cosine([1, 2], [1])
        with self.assertRaises(ValueError):
            compare.cosine([0], [0])

    def test_actual_protocol_and_missing_token_usage(self):
        code = '''import sys,json
print("[fake] ready",file=sys.stderr,flush=True)
for line in sys.stdin:
 r=json.loads(line)
 print(json.dumps({"id":r["id"],"token":"hello","done":True}),flush=True)
'''
        session = compare.Session([sys.executable, "-u", "-c", code], timeout=2)
        try:
            result = session.request({"type":"generate"})
            compare.check_result(result["events"], {"kind":"text"})
            self.assertIsNone(result["completion_tokens"])
            self.assertIsNone(result["tokens_per_second"])
            self.assertGreater(result["first_answer_seconds"], 0)
        finally:
            session.close()

    def test_unterminated_output_is_a_failure(self):
        session = compare.Session([sys.executable, "-u", "-c", '''import sys,json
print("[fake] ready",file=sys.stderr,flush=True)
r=json.loads(sys.stdin.readline())
print(json.dumps({"id":r["id"],"token":"partial"}),flush=True)
'''], timeout=2)
        try:
            with self.assertRaisesRegex(RuntimeError, "before completion"):
                session.request({"type":"generate"})
        finally:
            session.close()

    def test_failures_are_not_included_as_fast_successes(self):
        report = compare.report({"runs":[{"backend":"reference", "results":[
            {"workload":"text", "passed":True, "seconds":2},
            {"workload":"text", "passed":False, "seconds":0.01}]}]})
        self.assertIn("1/2 | 2.000000 | 2.000000", report)


if __name__ == "__main__":
    unittest.main()
