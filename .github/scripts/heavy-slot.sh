#!/usr/bin/env bash
# Take one of the shared Linux host's heavy-build slots for the rest of the
# job (#1242). The self-hosted runners share one VM's memory, and a backlog of
# Rust lanes and CodeQL Rust extractions starting together exhausted it and
# took every runner down. A background holder keeps the slot's flock until
# the job removes its marker, or until the runner reaps the job's processes,
# so a crashed or cancelled job never leaves a slot held.
#
# Usage: heavy-slot.sh <wait-seconds>
set -euo pipefail

wait_seconds=${1:?wait seconds}
slots=2
marker="$RUNNER_TEMP/cartograph-heavy-slot"
deadline=$((SECONDS + wait_seconds))

touch "$marker"
while ((SECONDS < deadline)); do
  for slot in $(seq "$slots"); do
    rm -f "$marker.held"
    flock -n "/tmp/cartograph-ci-heavy-$slot.lock" \
      sh -c 'touch "$1.held"; while [ -e "$1" ]; do sleep 5; done' _ "$marker" &
    holder=$!
    # flock -n exits at once when the slot is taken; otherwise the holder
    # marks the slot as held and keeps running.
    while kill -0 "$holder" 2>/dev/null && [ ! -e "$marker.held" ]; do sleep 0.2; done
    if [ -e "$marker.held" ]; then
      echo "Holding heavy-build slot $slot of $slots"
      exit 0
    fi
  done
  sleep 10
done
rm -f "$marker"
echo "::error::No heavy-build slot came free within ${wait_seconds}s"
exit 1
