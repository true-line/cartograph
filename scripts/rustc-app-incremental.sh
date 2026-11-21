#!/usr/bin/env bash
# RUSTC_WORKSPACE_WRAPPER for the app test build in CI (#1924).
#
# The app crate is one large crate that sccache cannot serve (a test harness
# is not cacheable), so every shard compiled all of it: about 2.5 minutes,
# most of each shard's time. This compiles only the app crate incrementally,
# into APP_INCREMENTAL_DIR, which ci.yml restores from the latest main build,
# so a pull request recompiles only what it changed. Every other workspace
# crate still goes through sccache, which cannot cache incremental builds.
#
# rustc exits 101 on an internal compiler error, the known failure mode of
# incremental state; the crate is then compiled once more without it. Ordinary
# compile errors exit 1 and are not retried.
set -euo pipefail

rustc=$1
shift
previous=
for argument in "$@"; do
  if [ "$previous" = --crate-name ] && [ "$argument" = app ] && [ -n "${APP_INCREMENTAL_DIR:-}" ]; then
    status=0
    "$rustc" "$@" -C "incremental=$APP_INCREMENTAL_DIR" || status=$?
    if [ "$status" -ne 101 ]; then
      exit "$status"
    fi
    echo "rustc-app-incremental: internal compiler error; compiling app again without incremental state" >&2
    rm -rf "$APP_INCREMENTAL_DIR"
    exec "$rustc" "$@"
  fi
  previous=$argument
done
exec "${SCCACHE_PATH:?SCCACHE_PATH is not set}" "$rustc" "$@"
