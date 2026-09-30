#!/usr/bin/env bash
# Nomos 3-Worker DAG Pipeline Demonstration via CLI
# Demonstrates running 3 workers with DAG dependencies using 'nomos run'
set -euo pipefail

echo "==============================================================================="
echo " NOMOS 3-WORKER DAG PIPELINE (CLI VERSION)"
echo "==============================================================================="

NOMOS_BIN="${NOMOS_BIN:-target/release/nomos}"
if [ ! -f "$NOMOS_BIN" ]; then
    NOMOS_BIN="target/debug/nomos"
fi

EXTRA_ARGS=()
if [ -n "${NOMOS_SOCKET:-}" ]; then
    EXTRA_ARGS+=(--socket "$NOMOS_SOCKET")
fi

echo "Using Nomos binary: $NOMOS_BIN"
echo "Launching Worker 1 (etl-preprocess: 2 cores, 4GB RAM) in background..."
"$NOMOS_BIN" run "${EXTRA_ARGS[@]}" --worker-id etl-preprocess --cpu 2.0 --mem 4GB -- sleep 2 &
PID1=$!

echo "Launching Worker 2 (feature-eng: 2 cores, 4GB RAM) in background..."
"$NOMOS_BIN" run "${EXTRA_ARGS[@]}" --worker-id feature-eng --cpu 2.0 --mem 4GB -- sleep 3 &
PID2=$!

sleep 0.5

echo "Launching Worker 3 (model-train: 3 cores, 8GB RAM, depends on etl-preprocess & feature-eng)..."
"$NOMOS_BIN" run "${EXTRA_ARGS[@]}" --worker-id model-train --depends-on etl-preprocess --depends-on feature-eng --cpu 3.0 --mem 8GB -- echo "Model training successfully executed after dependencies completed!" &
PID3=$!

# Wait for all workers to finish
wait $PID1
wait $PID2
wait $PID3

echo "==============================================================================="
echo "Pipeline completed! Checking Nomos accounting audit ledger:"
"$NOMOS_BIN" accounting "${EXTRA_ARGS[@]}" --limit 5
echo "==============================================================================="
