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

REGISTRY_ARGS=()
if [[ -n ${PUBLISH_REGISTRY:-} ]]; then
    REGISTRY_ARGS=(--registry "$PUBLISH_REGISTRY")
fi

publish_one() {
    local crate="$1" attempt
    for attempt in 1 2 3 4 5; do
        if (( DRY_RUN )); then
            cargo publish -p "$crate" --dry-run "${REGISTRY_ARGS[@]}" && return 0
        else
            cargo publish -p "$crate" "${REGISTRY_ARGS[@]}" && return 0
        fi
        echo "publish $crate failed (attempt $attempt); retrying in 20s" >&2
        sleep 20
    done
    return 1
}

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
    publish_one "$crate"
done
