#!/bin/sh
set -eu

cd /app
exec python3 -m vision_foundation.runtime
