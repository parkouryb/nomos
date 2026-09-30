#!/usr/bin/env bash
# ==============================================================================
# Nomos Network Isolation CLI Validation Script
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NOMOS_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "================================================================================"
echo " NOMOS NETWORK ISOLATION TEST (BASH RUNNER)"
echo "================================================================================"

# Build nomos if needed
echo "[INFO] Compiling Nomos binaries..."
cargo build --quiet --manifest-path "${NOMOS_ROOT}/Cargo.toml"

NOMOS_BIN="${NOMOS_ROOT}/target/debug/nomos"

# 1. Verify --network in help
echo "[INFO] Verifying CLI argument help output..."
"${NOMOS_BIN}" run --help | grep -q -- "--network"
echo "[PASS] --network flag present in 'nomos run --help'"

# 2. Verify invalid network mode is rejected
echo "[INFO] Testing invalid network mode handling..."
set +e
"${NOMOS_BIN}" run --network invalid_mode -- echo "fail" 2>/dev/null
EXIT_CODE=$?
set -e
if [[ ${EXIT_CODE} -ne 0 ]]; then
    echo "[PASS] Invalid network mode rejected (exit code: ${EXIT_CODE})"
else
    echo "[FAIL] Invalid network mode was unexpectedly accepted"
    exit 1
fi

# 3. Launch temporary arbiter daemon for end-to-end tests
TMP_DIR="$(mktemp -d /tmp/nomos_net_test_XXXXXX)"
SOCK="${TMP_DIR}/arbiter.sock"
CONF="${TMP_DIR}/nomos.toml"

cat << 'EOF' > "${CONF}"
[limits]
reserved_cores = 1.0
reserved_memory_bytes = 1073741824
storage_scratch_dir = "/tmp/nomos-scratch"
EOF

echo "[INFO] Starting test arbiter daemon..."
"${NOMOS_BIN}" daemon --config "${CONF}" --socket "${SOCK}" --port 9198 &
DAEMON_PID=$!

cleanup() {
    echo "[INFO] Cleaning up test daemon..."
    kill -TERM "${DAEMON_PID}" 2>/dev/null || true
    wait "${DAEMON_PID}" 2>/dev/null || true
    rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

# Wait for socket
for i in {1..30}; do
    if [[ -S "${SOCK}" ]]; then
        break
    fi
    sleep 0.1
done

if [[ ! -S "${SOCK}" ]]; then
    echo "[FAIL] Daemon socket failed to start."
    exit 1
fi
echo "[PASS] Test daemon is listening on ${SOCK}"

# 4. Test run with mode 'isolated'
echo "[INFO] Testing 'nomos run --network isolated'..."
"${NOMOS_BIN}" run --socket "${SOCK}" --network isolated -- echo "nomos isolated ok" | grep -q "nomos isolated ok"
echo "[PASS] Mode 'isolated' executed cleanly"

# 5. Test run with mode 'none'
echo "[INFO] Testing 'nomos run --network none'..."
"${NOMOS_BIN}" run --socket "${SOCK}" --network none -- echo "nomos none ok" | grep -q "nomos none ok"
echo "[PASS] Mode 'none' executed cleanly"

# 6. Test run with mode 'host'
echo "[INFO] Testing 'nomos run --network host'..."
"${NOMOS_BIN}" run --socket "${SOCK}" --network host -- echo "nomos host ok" | grep -q "nomos host ok"
echo "[PASS] Mode 'host' executed cleanly"

# 7. Run python test suite
if command -v python3 >/dev/null 2>&1; then
    echo "[INFO] Running Python test suite..."
    python3 "${SCRIPT_DIR}/test_network_isolation.py"
fi

echo "================================================================================"
echo "[SUCCESS] All Nomos Network Isolation tests passed!"
echo "================================================================================"
