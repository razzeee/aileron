#!/bin/sh
set -eu

default_mmproj_path="${MMPROJ_PATH:-/model/mmproj.gguf}"
if [ -f "$default_mmproj_path" ]; then
    export MMPROJ_PATH="$default_mmproj_path"
    : "${MODEL_PATH:=/model/model.gguf}"
    export MODEL_PATH
    exec /usr/local/bin/aileron-runtime-vision-llama-cpp
fi

if [ -z "${MODEL_PATH:-}" ]; then
    if [ -f /model/model.bin ]; then
        MODEL_PATH=/model/model.bin
    else
        MODEL_PATH=/model/model.gguf
    fi
    export MODEL_PATH
fi

case "$MODEL_PATH" in
    *.bin) exec /usr/local/bin/aileron-runtime-asr-whisper-cpp ;;
    *) exec /usr/local/bin/aileron-runtime-llm-llama-cpp ;;
esac
