#!/bin/sh
set -eu
cd /app
exec python3 -m tts_vits.runtime
