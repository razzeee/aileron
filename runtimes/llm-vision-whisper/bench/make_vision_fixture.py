"""Generate a small deterministic visual conformance workload. Requires Pillow."""
import argparse
import base64
import io
import json
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--operation", choices=["describe","ocr","detect"])
    parser.add_argument("--format", choices=["PNG","JPEG"], default="PNG")
    args = parser.parse_args()
    image = Image.new("RGB", (320, 160), "white")
    draw = ImageDraw.Draw(image)
    draw.text((20, 15), "HELLO 123", fill="black", font=ImageFont.load_default(size=36))
    draw.rectangle((30, 80, 110, 145), fill="red")
    draw.ellipse((190, 80, 255, 145), fill="blue")
    data = io.BytesIO()
    image.save(data, format=args.format)
    encoded = base64.b64encode(data.getvalue()).decode()
    workloads = [
        {"name":"describe", "request":{"type":"describe","image":encoded,"max_tokens":128},"expect":{"kind":"text","contains_all":["red","blue"],"forbid_any":["provide the image"]}},
        {"name":"ocr", "request":{"type":"ocr","image":encoded,"max_tokens":128},"expect":{"kind":"text","contains_all":["HELLO","123"]}},
        {"name":"detect", "request":{"type":"detect","image":encoded,"max_tokens":256},"expect":{"kind":"detections"}},
    ]
    if args.operation:
        workloads=[workload for workload in workloads if workload["name"]==args.operation]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(workloads, indent=2) + "\n")
    image.save(args.output.with_suffix(".png"))


if __name__ == "__main__":
    main()
