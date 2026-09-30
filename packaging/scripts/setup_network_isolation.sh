#!/usr/bin/env bash
# ==============================================================================
# NOMOS NETWORK EGRESS ISOLATION SETUP SCRIPT
# Sets up Linux Cgroups v2 hierarchy and Netfilter (iptables / nftables) rules
# for deterministic network isolation modes:
#   - isolated: Loopback (127.0.0.1) & RFC1918 LAN only (Blocks WAN)
#   - none:     Air-gapped (Loopback only, blocks all LAN/WAN)
#   - host:     Unrestricted normal host network
# ==============================================================================

set -euo pipefail

CGROUP_ROOT="/sys/fs/cgroup"
NOMOS_CGROUP_BASE="${CGROUP_ROOT}/nomos"
NOMOS_USER="${NOMOS_USER:-${SUDO_USER:-$(id -un)}}"
FIREWALL_BACKEND="${NOMOS_FIREWALL_BACKEND:-auto}"

print_header() {
    echo "================================================================================"
    echo " Nomos Network Egress Isolation - Linux Security Setup"
    echo "================================================================================"
}

check_root() {
    if [[ $EUID -ne 0 ]]; then
        echo "[ERROR] This script must be executed as root (e.g. sudo bash $0)." >&2
        exit 1
    fi
}

check_cgroups_v2() {
    if [[ ! -d "${CGROUP_ROOT}" ]]; then
        echo "[ERROR] /sys/fs/cgroup directory not found." >&2
        exit 1
    fi

    # Check if cgroup v2 is mounted
    if ! grep -q "cgroup2" /proc/mounts; then
        echo "[WARNING] Unified Cgroup v2 not detected in /proc/mounts." >&2
        echo "          Ensure systemd.unified_cgroup_hierarchy=1 is active in kernel cmdline." >&2
    fi
}

detect_backend() {
    if [[ "${FIREWALL_BACKEND}" == "auto" ]]; then
        if command -v nft >/dev/null 2>&1; then
            FIREWALL_BACKEND="nftables"
        elif command -v iptables >/dev/null 2>&1; then
            FIREWALL_BACKEND="iptables"
        else
            echo "[ERROR] Neither 'nft' nor 'iptables' found on this system." >&2
            exit 1
        fi
    fi
    echo "[INFO] Using firewall backend: ${FIREWALL_BACKEND}"
}

setup_cgroups() {
    echo "[INFO] Configuring Cgroups v2 hierarchy at ${NOMOS_CGROUP_BASE}..."

    mkdir -p "${NOMOS_CGROUP_BASE}/isolated"
    mkdir -p "${NOMOS_CGROUP_BASE}/none"
    mkdir -p "${NOMOS_CGROUP_BASE}/host"

    # Enable controllers in parent if available
    if [[ -f "${CGROUP_ROOT}/cgroup.controllers" ]]; then
        for ctrl in cpu memory io; do
            if grep -q "\b${ctrl}\b" "${CGROUP_ROOT}/cgroup.controllers" 2>/dev/null; then
                echo "+${ctrl}" > "${CGROUP_ROOT}/cgroup.subtree_control" 2>/dev/null || true
                echo "+${ctrl}" > "${NOMOS_CGROUP_BASE}/cgroup.subtree_control" 2>/dev/null || true
                echo "+${ctrl}" > "${NOMOS_CGROUP_BASE}/isolated/cgroup.subtree_control" 2>/dev/null || true
                echo "+${ctrl}" > "${NOMOS_CGROUP_BASE}/none/cgroup.subtree_control" 2>/dev/null || true
                echo "+${ctrl}" > "${NOMOS_CGROUP_BASE}/host/cgroup.subtree_control" 2>/dev/null || true
            fi
        done
    fi

    # Delegate ownership to Nomos service user if specified
    if id "${NOMOS_USER}" >/dev/null 2>&1; then
        echo "[INFO] Delegating cgroup permissions to user '${NOMOS_USER}'..."
        chown -R "${NOMOS_USER}:${NOMOS_USER}" "${NOMOS_CGROUP_BASE}" 2>/dev/null || true
    fi
    chmod -R 0775 "${NOMOS_CGROUP_BASE}" 2>/dev/null || true

    echo "[INFO] Cgroup hierarchy initialized:"
    echo "       - ${NOMOS_CGROUP_BASE}/isolated"
    echo "       - ${NOMOS_CGROUP_BASE}/none"
    echo "       - ${NOMOS_CGROUP_BASE}/host"
}

apply_iptables_rules() {
    echo "[INFO] Applying iptables cgroup matching rules..."
    modprobe xt_cgroup 2>/dev/null || true

    # Create dedicated NOMOS_EGRESS chain if missing
    iptables -N NOMOS_EGRESS 2>/dev/null || true

    # Link from OUTPUT chain
    if ! iptables -C OUTPUT -j NOMOS_EGRESS 2>/dev/null; then
        iptables -I OUTPUT 1 -j NOMOS_EGRESS
    fi

    # Flush existing rules inside NOMOS_EGRESS for idempotency
    iptables -F NOMOS_EGRESS

    # 1. Allow already established / related sessions
    iptables -A NOMOS_EGRESS -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT

    # 2. Mode 'none' (Air-Gapped): Loopback only, reject all other destinations
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/none" -o lo -j ACCEPT
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/none" -d 127.0.0.0/8 -j ACCEPT
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/none" -j REJECT --reject-with icmp-admin-prohibited

    # 3. Mode 'isolated': Loopback + RFC1918 Private LAN subnets, reject WAN
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/isolated" -o lo -j ACCEPT
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/isolated" -d 127.0.0.0/8 -j ACCEPT
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/isolated" -d 10.0.0.0/8 -j ACCEPT
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/isolated" -d 172.16.0.0/12 -j ACCEPT
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/isolated" -d 192.168.0.0/16 -j ACCEPT
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/isolated" -d 169.254.0.0/16 -j ACCEPT
    iptables -A NOMOS_EGRESS -m cgroup --path "nomos/isolated" -j REJECT --reject-with icmp-admin-prohibited

    # 4. Mode 'host': Unrestricted (passes through to default OUTPUT policy)
    echo "[INFO] iptables rules successfully applied."
}

apply_nftables_rules() {
    echo "[INFO] Applying nftables cgroup socket rules..."

    cat << 'EOF' | nft -f -
table inet nomos_egress {
    chain output {
        type filter hook output priority 0; policy accept;

        # Keep existing established connections
        ct state established,related accept

        # Air-gapped Mode (none): Only loopback allowed
        socket cgroupv2 "nomos/none" oif "lo" accept
        socket cgroupv2 "nomos/none" ip daddr 127.0.0.0/8 accept
        socket cgroupv2 "nomos/none" reject with icmpx type admin-prohibited

        # Isolated Mode: Loopback and RFC1918 LAN only, block WAN
        socket cgroupv2 "nomos/isolated" oif "lo" accept
        socket cgroupv2 "nomos/isolated" ip daddr { 127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16 } accept
        socket cgroupv2 "nomos/isolated" reject with icmpx type admin-prohibited
    }
}
EOF
    echo "[INFO] nftables rules successfully applied."
}

teardown_rules() {
    echo "[INFO] Tearing down Nomos network isolation rules..."
    detect_backend

    if [[ "${FIREWALL_BACKEND}" == "iptables" ]] || command -v iptables >/dev/null 2>&1; then
        iptables -D OUTPUT -j NOMOS_EGRESS 2>/dev/null || true
        iptables -F NOMOS_EGRESS 2>/dev/null || true
        iptables -X NOMOS_EGRESS 2>/dev/null || true
        echo "[INFO] Flushed and removed iptables NOMOS_EGRESS chain."
    fi

    if [[ "${FIREWALL_BACKEND}" == "nftables" ]] || command -v nft >/dev/null 2>&1; then
        nft delete table inet nomos_egress 2>/dev/null || true
        echo "[INFO] Removed nftables table inet nomos_egress."
    fi

    echo "[INFO] Network isolation rules teardown complete."
}

show_status() {
    print_header
    echo "Cgroup V2 Directory: ${NOMOS_CGROUP_BASE}"
    if [[ -d "${NOMOS_CGROUP_BASE}" ]]; then
        echo "Cgroup slices:"
        ls -la "${NOMOS_CGROUP_BASE}"
    else
        echo "Status: Not configured"
    fi
    echo ""

    detect_backend
    if [[ "${FIREWALL_BACKEND}" == "iptables" ]]; then
        echo "Active iptables NOMOS_EGRESS rules:"
        iptables -L NOMOS_EGRESS -v -n --line-numbers 2>/dev/null || echo "Chain NOMOS_EGRESS does not exist."
    elif [[ "${FIREWALL_BACKEND}" == "nftables" ]]; then
        echo "Active nftables nomos_egress table:"
        nft list table inet nomos_egress 2>/dev/null || echo "Table inet nomos_egress does not exist."
    fi
}

main() {
    local cmd="${1:-apply}"

    case "${cmd}" in
        apply|install)
            check_root
            print_header
            check_cgroups_v2
            detect_backend
            setup_cgroups
            if [[ "${FIREWALL_BACKEND}" == "nftables" ]]; then
                apply_nftables_rules
            else
                apply_iptables_rules
            fi
            echo "[SUCCESS] Nomos network egress isolation is active."
            ;;
        teardown|flush|cleanup)
            check_root
            print_header
            teardown_rules
            ;;
        status)
            show_status
            ;;
        help|--help|-h)
            print_header
            echo "Usage: $0 [apply|teardown|status|help]"
            echo ""
            echo "Commands:"
            echo "  apply     Setup cgroups v2 hierarchy and install firewall rules (default)"
            echo "  teardown  Remove firewall rules and clean up Nomos netfilter chains"
            echo "  status    Show active cgroups and firewall filtering status"
            echo "  help      Show this help message"
            echo ""
            echo "Environment Variables:"
            echo "  NOMOS_FIREWALL_BACKEND  'auto', 'iptables', or 'nftables'"
            echo "  NOMOS_USER              Service user owning the cgroup subtree"
            ;;
        *)
            echo "[ERROR] Unknown command: ${cmd}" >&2
            echo "Run '$0 help' for available commands." >&2
            exit 1
            ;;
    esac
}

main "$@"
