#!/bin/sh
# Runs each fuzz target briefly: ./fuzz/run.sh [seconds per target, default 30]
set -eu
cd "$(dirname "$0")/.."
secs="${1:-30}"
export TMPDIR="${TMPDIR:-$PWD/target/tmp}"
mkdir -p "$TMPDIR"
for t in catalog_json tar zip; do
    mkdir -p "fuzz/corpus/$t"
    cargo fuzz run "$t" "fuzz/corpus/$t" -- -max_total_time="$secs" -rss_limit_mb=2048
done
