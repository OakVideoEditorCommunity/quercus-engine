#!/usr/bin/env bash
# Publish the Quercus engine workspace to crates.io in dependency order.
#
#   tooling/publish_workspace.sh --dry-run
#   tooling/publish_workspace.sh
#
# Every crate declares an exact version on its internal path dependencies;
# cargo refuses to package a crate whose dependency version is not registered
# yet, so the order matters.

set -euo pipefail
cd "$(dirname "$0")/.."

DRY_RUN=0
if [[ ${1:-} == "--dry-run" ]]; then
    DRY_RUN=1
fi

PUBLISH_REGISTRY="${PUBLISH_REGISTRY:-crates-io}"
REGISTRY_ARGS=(--registry "$PUBLISH_REGISTRY")

publish_one() {
    local crate="$1" attempt output pkgid version search
    pkgid="$(cargo pkgid -p "$crate")"
    version="${pkgid##*#}"
    # Avoid another upload attempt for a version that already exists. Query
    # the real crates.io registry explicitly (a source replacement such as
    # ustc cannot answer `cargo search` for this purpose).
    search="$(cargo search "$crate" --limit 1 "${REGISTRY_ARGS[@]}" 2>/dev/null || true)"
    if [[ "$search" == "$crate = \"$version\""* ]]; then
        echo "$crate@$version is already published; skipping"
        return 0
    fi
    for attempt in 1 2 3 4 5; do
        if (( DRY_RUN )); then
            cargo publish -p "$crate" --dry-run "${REGISTRY_ARGS[@]}" && return 0
        else
            if output="$(cargo publish -p "$crate" "${REGISTRY_ARGS[@]}" 2>&1)"; then
                printf '%s\n' "$output"
                return 0
            fi
            printf '%s\n' "$output"
            if grep -Eq 'already uploaded|already exists' <<<"$output"; then
                echo "$crate is already published; skipping"
                return 0
            fi
        fi
        echo "publish $crate failed (attempt $attempt); retrying in 20s" >&2
        sleep 20
    done
    return 1
}

ORDER=(
    quercus-core
    quercus-undo
    quercus-ffmpeg-link
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
