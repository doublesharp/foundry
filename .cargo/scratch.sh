#!/bin/sh
# Compatibility entry point using the saved checkout configuration.
set -eu
exec python3 "$(dirname "$0")/../.github/scripts/scratch.py" run "$@"
