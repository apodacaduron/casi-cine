#!/bin/sh
set -eu
TARGET="$1"
echo "SOURCE Downloading ACE-Step source"
mkdir -p "$(dirname "$TARGET")"
if [ ! -d "$TARGET/.git" ]; then git clone --depth 1 https://github.com/ace-step/ACE-Step-1.5.git "$TARGET"; fi
echo "ENV Installing isolated Python 3.12 environment"
cd "$TARGET"
uv sync --python 3.12
echo "MODEL Downloading approximately 10 GB of model assets"
uv run acestep-download
echo "DONE ACE-Step is ready"
