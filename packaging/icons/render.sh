#!/bin/sh
# Draws Stockroom's icon and exports its sizes (see render.py). Needs resvg, and iconutil
# (macOS) for stockroom.icns.
set -eu
cd "$(dirname "$0")"
exec python3 -I render.py
