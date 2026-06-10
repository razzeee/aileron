#!/usr/bin/env python3
"""Download a Whisper model via pywhispercpp into /model at build time."""
import os
import sys

MODEL_SIZE = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("MODEL_SIZE", "base")
os.makedirs("/model", exist_ok=True)

print(f"[aileron-asr] downloading whisper model: {MODEL_SIZE}", flush=True)
from pywhispercpp.model import Model
Model(MODEL_SIZE, models_dir="/model")
print(f"[aileron-asr] done: /model", flush=True)
