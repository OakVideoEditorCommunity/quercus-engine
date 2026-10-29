#!/usr/bin/env bash
# Publish the Quercus engine workspace to crates.io in dependency order.
#
#   tooling/publish_workspace.sh --dry-run
#   tooling/publish_workspace.sh
#
# Every crate declares `version = "=0.5.2"` on its internal path dependencies;
# cargo refuses to package a crate whose dependency version is not registered
# yet, so the order matters.

set -euo pipefail
cd "$(dirname "$0")/.."

DRY_RUN=0
if [[ ${1:-} == "--dry-run" ]]; then
    DRY_RUN=1
fi

ORDER=(
    quercus-core
    quercus-ffmpeg-link
    quercus-undo
    quercus-codec
    quercus-node
    quercus-otio
    quercus-render
    quercus-audio
    quercus-timeline
    quercus-storage
    quercus-task
    quercus-ofx-plugin
    quercus-engine
    quercus-engine-rs
)

for crate in "${ORDER[@]}"; do
    echo "==> $crate"
    if (( DRY_RUN )); then
        cargo publish -p "$crate" --dry-run
    else
        cargo publish -p "$crate"
    fi
done
