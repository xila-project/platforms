#!/bin/sh
# Prints one build information value for the [env] templates of mise.toml, so
# that every build of this repository carries it:
#   build_information.sh commit   short commit of HEAD, with a -dirty suffix
#                                 when the worktree has changes
#   build_information.sh epoch    commit time of HEAD, as SOURCE_DATE_EPOCH
# Falls back to unknown and 0 outside a Git checkout.
set -eu

case "${1:-}" in
    commit)
        commit=$(git rev-parse --short HEAD 2>/dev/null || echo unknown)
        if [ "$commit" != unknown ] && [ -n "$(git status --porcelain 2>/dev/null)" ]; then
            commit="$commit-dirty"
        fi
        echo "$commit"
        ;;
    epoch)
        git log -1 --format=%ct 2>/dev/null || echo 0
        ;;
    *)
        echo "Usage: $0 commit|epoch" >&2
        exit 2
        ;;
esac
