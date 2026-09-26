"""Compare server embeddings across graph growth and generation mode switches."""
import argparse
import json
from pathlib import Path

from compare import command_json, cosine, file_hash, run_image


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-image", required=True)
    parser.add_argument("--candidate-image", required=True)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--variant", choices=["cpu", "vulkan", "cuda", "rocm"], default="cpu")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    workloads = []
    for repeats in (1, 128, 384, 1):
        workloads.extend([
            {"name": f"embedding-{repeats}",
             "request": {"type": "embed", "prompt": "A small cat sits on a mat. " * repeats},
             "expect": {"kind": "embedding"}},
            {"name": f"generation-{repeats}",
             "request": {"type": "generate", "prompt": "Say hello briefly.", "max_tokens": 16, "temperature": 0.0},
             "expect": {"kind": "text", "contains_any": ["hello", "hi"]}},
        ])
    images = {name: command_json("podman", "image", "inspect", image)[0]["Id"]
              for name, image in [("reference", args.reference_image), ("candidate", args.candidate_image)]}
    # Both images are servers. The reference is the pre-optimization server,
    # which evaluates the full embedding batch with the original reservation.
    runs = {name: run_image(image, args.model_dir.resolve(), workloads, "candidate", 1, 4096, 2, args.variant)
            for name, image in images.items()}
    agreements = []
    for reference, candidate in zip(runs["reference"]["results"], runs["candidate"]["results"], strict=True):
        if reference["passed"] and candidate["passed"] and reference["workload"].startswith("embedding-"):
            agreements.append(cosine(reference["events"][-1]["embedding"], candidate["events"][-1]["embedding"]))
    report = {"images": images, "model_sha256": file_hash(args.model_dir / "model.gguf"),
              "runs": runs, "embedding_cosines": agreements}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    assert all(result["passed"] for run in runs.values() for result in run["results"]), "operation failed; inspect report"
    assert len(agreements) == 4 and min(agreements) >= 0.9999, agreements
    print(f"All operations passed; embedding cosines: {agreements}")


if __name__ == "__main__":
    main()
