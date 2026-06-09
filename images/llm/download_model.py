#!/usr/bin/env python3
"""Download a GGUF model from a HuggingFace resolve URL into /model/model.gguf."""
import os
import sys
import urllib.parse
import urllib.request

def download(url: str, dest: str) -> None:
    """Stream-download url to dest, printing a progress bar to stdout."""
    # Follow redirects (HuggingFace CDN redirects to S3/Cloudflare).
    req = urllib.request.Request(url, headers={"User-Agent": "aileron/1.0"})
    with urllib.request.urlopen(req) as resp:
        total = int(resp.headers.get("Content-Length", 0))
        chunk = 1024 * 1024  # 1 MiB
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
    print(flush=True)  # newline after bar


url = sys.argv[1]
filename = urllib.parse.urlparse(url).path.split("/")[-1]
dest = "/model/model.gguf"

os.makedirs("/model", exist_ok=True)
print(f"Downloading {filename} -> {dest}", flush=True)
download(url, dest)
print(f"Done: {dest}", flush=True)
