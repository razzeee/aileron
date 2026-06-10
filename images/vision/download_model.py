#!/usr/bin/env python3
"""Download a multimodal GGUF model and mmproj file into /model."""
import os
import sys
import urllib.parse
import urllib.request


def download(url: str, dest: str) -> None:
    req = urllib.request.Request(url, headers={"User-Agent": "aileron/1.0"})
    with urllib.request.urlopen(req) as resp:
        total = int(resp.headers.get("Content-Length", 0))
        chunk = 1024 * 1024
        written = 0
        with open(dest, "wb") as f:
            while True:
                buf = resp.read(chunk)
                if not buf:
                    break
                f.write(buf)
                written += len(buf)
                if total:
                    pct = written * 100 // total
                    bar = "#" * (pct // 2) + "-" * (50 - pct // 2)
                    print(
                        f"\r  [{bar}] {pct:3d}%"
                        f"  {written / 1024**2:.0f} / {total / 1024**2:.0f} MiB",
                        end="", flush=True,
                    )
                else:
                    print(f"\r  {written / 1024**2:.0f} MiB downloaded",
                          end="", flush=True)
    print(flush=True)


def maybe_download(url: str, dest: str) -> None:
    if not url:
        return
    filename = urllib.parse.urlparse(url).path.split("/")[-1]
    print(f"Downloading {filename} -> {dest}", flush=True)
    download(url, dest)
    print(f"Done: {dest}", flush=True)


model_url = sys.argv[1] if len(sys.argv) > 1 else ""
mmproj_url = sys.argv[2] if len(sys.argv) > 2 else ""

os.makedirs("/model", exist_ok=True)
maybe_download(model_url, "/model/model.gguf")
maybe_download(mmproj_url, "/model/mmproj.gguf")
