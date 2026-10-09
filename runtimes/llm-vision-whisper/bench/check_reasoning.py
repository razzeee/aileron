"""Conformance only: do not interpret this run as a performance comparison."""
import argparse
import json
from pathlib import Path
from compare import command_json, file_hash, run_image


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image",required=True)
    parser.add_argument("--model-dir",type=Path,required=True)
    parser.add_argument("--workloads",type=Path,required=True)
    parser.add_argument("--memory",default="8g")
    parser.add_argument("--variant",choices=["cpu","vulkan","cuda","rocm"],default="cpu")
    parser.add_argument("--output",type=Path,required=True)
    args=parser.parse_args()
    info=command_json("podman","image","inspect",args.image)[0]
    labels=info.get("Config",{}).get("Labels") or {}
    if labels.get("org.aileron.variant") != args.variant:
        parser.error("image variant does not match the requested accelerator")
    data={"image":info["Id"],"model_sha256":file_hash(args.model_dir/"model.gguf"),
          "workloads_sha256":file_hash(args.workloads),"variant":args.variant}
    args.output.parent.mkdir(parents=True,exist_ok=True)
    data["run"]=run_image(info["Id"],args.model_dir.resolve(),json.loads(args.workloads.read_text()),
                          "candidate",1,4096,2,variant=args.variant,memory=args.memory)
    args.output.write_text(json.dumps(data,indent=2)+"\n")
    print([(item["workload"],item["passed"],item.get("error")) for item in data["run"]["results"]])
    if not all(item["passed"] for item in data["run"]["results"]):
        raise SystemExit(1)


if __name__=="__main__": main()
