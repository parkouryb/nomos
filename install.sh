#!/usr/bin/env bash
# ==============================================================================
# NOMOS (Νόμος) — One-Command Universal Installer & Updater
# Supports: macOS (Apple Silicon / Intel) & Linux (Ubuntu / Debian / Systemd)
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_NAME="nomos"
INSTALL_DIR="${HOME}/.local/bin"
CONFIG_DIR="${HOME}/.nomos"
OS="$(uname -s)"
ARCH="$(uname -m)"
RELEASE_BIN="${SCRIPT_DIR}/target/release/${BIN_NAME}"

echo "==============================================================================="
echo " NOMOS SINGLE-HOST RESOURCE ARBITER — INSTALLER & UPDATER"
echo " Detected System: ${OS} (${ARCH})"
echo "==============================================================================="

# 1. Verify required toolchain
if ! command -v cargo >/dev/null 2>&1; then
    if [ -f "${HOME}/.cargo/env" ]; then
        # shellcheck disable=SC1091
        source "${HOME}/.cargo/env"
    fi
fi

if ! command -v cargo >/dev/null 2>&1; then
    echo "[ERROR] Cargo/Rust toolchain not found."
    echo "Please install Rust first via: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
    echo "[WARNING] Python 3 not found. Python SDK installation will be skipped."
fi

# 2. Stop running daemon service if active to avoid 'text file busy' errors
echo "[1/6] Stopping active Nomos daemon services (if running)..."
if [ "${OS}" = "Linux" ]; then
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user stop nomos.service 2>/dev/null || true
    fi
elif [ "${OS}" = "Darwin" ]; then
    if [ -f "${HOME}/Library/LaunchAgents/com.nomos.arbiter.plist" ]; then
        launchctl unload "${HOME}/Library/LaunchAgents/com.nomos.arbiter.plist" 2>/dev/null || true
    fi
fi
pkill -f "nomos daemon" 2>/dev/null || true
sleep 0.5

# 3. Compile or verify release binary
echo "[2/6] Compiling Nomos release binary with native optimizations..."
cd "${SCRIPT_DIR}"

# Safely rename existing target binary so cargo builds a new inode
if [ -f "${RELEASE_BIN}" ]; then
    rm -f "${RELEASE_BIN}.old" 2>/dev/null || true
    mv -f "${RELEASE_BIN}" "${RELEASE_BIN}.old" 2>/dev/null || true
fi

cargo build --release --package nomos

if [ ! -f "${RELEASE_BIN}" ]; then
    echo "[ERROR] Compilation failed: binary not found at ${RELEASE_BIN}"
    exit 1
fi
rm -f "${RELEASE_BIN}.old" 2>/dev/null || true

# 4. Install binary to user PATH atomically
echo "[3/6] Installing ${BIN_NAME} executable to ${INSTALL_DIR} and ~/.cargo/bin..."
mkdir -p "${INSTALL_DIR}" "${HOME}/.cargo/bin"

# Atomic install to ~/.local/bin
rm -f "${INSTALL_DIR}/${BIN_NAME}.tmp" 2>/dev/null || true
cp -f "${RELEASE_BIN}" "${INSTALL_DIR}/${BIN_NAME}.tmp"
chmod +x "${INSTALL_DIR}/${BIN_NAME}.tmp"
mv -f "${INSTALL_DIR}/${BIN_NAME}.tmp" "${INSTALL_DIR}/${BIN_NAME}"

# Atomic install to ~/.cargo/bin
rm -f "${HOME}/.cargo/bin/${BIN_NAME}.tmp" 2>/dev/null || true
cp -f "${RELEASE_BIN}" "${HOME}/.cargo/bin/${BIN_NAME}.tmp"
chmod +x "${HOME}/.cargo/bin/${BIN_NAME}.tmp"
mv -f "${HOME}/.cargo/bin/${BIN_NAME}.tmp" "${HOME}/.cargo/bin/${BIN_NAME}"

# Ensure PATH in shell configs
setup_path_in_file() {
    local file="$1"
    if [ -f "${file}" ]; then
        if ! grep -q 'nomos_path_setup' "${file}" 2>/dev/null; then
            # Prepend before non-interactive shell check if present
            if grep -q 'case \$- in' "${file}" 2>/dev/null; then
                sed -i.bak '1s|^|# nomos_path_setup\nexport PATH=\"\$HOME/.local/bin:\$HOME/.cargo/bin:\$PATH\"\n|' "${file}"
                rm -f "${file}.bak"
            else
                echo -e '\n# nomos_path_setup\nexport PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"' >> "${file}"
            fi
            echo "  - Added ${INSTALL_DIR} to ${file}"
        fi
    fi
}

setup_path_in_file "${HOME}/.bashrc"
setup_path_in_file "${HOME}/.zshrc"
setup_path_in_file "${HOME}/.profile"
export PATH="${INSTALL_DIR}:${HOME}/.cargo/bin:${PATH}"

# 5. Initialize configuration
echo "[4/6] Setting up configuration at ${CONFIG_DIR}/nomos.toml..."
mkdir -p "${CONFIG_DIR}"
if [ ! -f "${CONFIG_DIR}/nomos.toml" ]; then
    cp "${SCRIPT_DIR}/nomos.toml" "${CONFIG_DIR}/nomos.toml"
    echo "  - Created default configuration: ${CONFIG_DIR}/nomos.toml"
else
    echo "  - Preserving existing configuration: ${CONFIG_DIR}/nomos.toml"
fi

# 6. Install Python SDK
if command -v python3 >/dev/null 2>&1; then
    echo "[5/6] Installing / Updating Nomos Python Client SDK..."
    python3 -m pip install -e "${SCRIPT_DIR}/sdk/python" --break-system-packages 2>/dev/null || \
    python3 -m pip install --user -e "${SCRIPT_DIR}/sdk/python" 2>/dev/null || \
    python3 -m pip install -e "${SCRIPT_DIR}/sdk/python" 2>/dev/null || \
    echo "  [WARNING] Python pip install failed, SDK available directly at ${SCRIPT_DIR}/sdk/python"
fi

# 7. Configure and start background daemon service
echo "[6/6] Configuring and activating background daemon service..."
if [ "${OS}" = "Linux" ]; then
    SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
    mkdir -p "${SYSTEMD_USER_DIR}"
    cat << EOF > "${SYSTEMD_USER_DIR}/nomos.service"
[Unit]
Description=Nomos Single-Host Delicate Resource Arbiter
After=default.target

[Service]
Type=simple
WorkingDirectory=%h
ExecStart=${INSTALL_DIR}/${BIN_NAME} daemon --config ${CONFIG_DIR}/nomos.toml --port 9100 --socket ${CONFIG_DIR}/arbiter.sock
Restart=always
RestartSec=3

[Install]
WantedBy=default.target
EOF

    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user daemon-reload
        systemctl --user enable --now nomos.service
        echo "  - Systemd user service active: nomos.service"
    fi

elif [ "${OS}" = "Darwin" ]; then
    LAUNCHD_DIR="${HOME}/Library/LaunchAgents"
    mkdir -p "${LAUNCHD_DIR}"
    cat << EOF > "${LAUNCHD_DIR}/com.nomos.arbiter.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.nomos.arbiter</string>
    <key>ProgramArguments</key>
    <array>
        <string>${INSTALL_DIR}/${BIN_NAME}</string>
        <string>daemon</string>
        <string>--config</string>
        <string>${CONFIG_DIR}/nomos.toml</string>
        <string>--port</string>
        <string>9100</string>
        <string>--socket</string>
        <string>${CONFIG_DIR}/arbiter.sock</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>${CONFIG_DIR}/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>${CONFIG_DIR}/daemon.err</string>
    <key>WorkingDirectory</key>
    <string>${HOME}</string>
</dict>
</plist>
EOF
    launchctl unload "${LAUNCHD_DIR}/com.nomos.arbiter.plist" 2>/dev/null || true
    launchctl load -w "${LAUNCHD_DIR}/com.nomos.arbiter.plist"
    echo "  - macOS LaunchAgent active: com.nomos.arbiter"
fi

# 8. Verification
echo "-------------------------------------------------------------------------------"
echo "Verifying Nomos Arbiter Daemon response..."
sleep 1.5

VERIFIED=false
for _ in {1..15}; do
    if [ -S "${CONFIG_DIR}/arbiter.sock" ]; then
        VERIFIED=true
        break
    fi
    sleep 0.2
done

if [ "${VERIFIED}" = "true" ]; then
    "${INSTALL_DIR}/${BIN_NAME}" status
    echo "==============================================================================="
    echo " INSTALLATION / UPDATE COMPLETE!"
    echo " Executable:  ${INSTALL_DIR}/${BIN_NAME} ($("${INSTALL_DIR}/${BIN_NAME}" --version))"
    echo " Socket:      ${CONFIG_DIR}/arbiter.sock"
    echo " Config:      ${CONFIG_DIR}/nomos.toml"
    echo " Web UI:      http://localhost:9100"
    echo " Python SDK:  import nomos ($("${INSTALL_DIR}/${BIN_NAME}" --version))"
    echo "==============================================================================="
else
    echo "[WARNING] Daemon socket not ready yet. Please check: ${INSTALL_DIR}/${BIN_NAME} status"
fi
