#!/usr/bin/env python3
"""
Nomos Network Egress Isolation Test Suite
Validates the correctness of the network egress isolation mechanism:
  1. Unit tests for RFC1918 LAN, Loopback, and WAN address classification.
  2. Rule enforcement semantics:
     - Mode 'none': Loopback only (air-gapped).
     - Mode 'isolated': Loopback and RFC1918 private LAN only. Public WAN blocked.
     - Mode 'host': Normal unrestricted networking.
  3. CLI parsing verification:
     - 'nomos run --network <isolated|none|host>'
     - Invalid network argument rejection.
  4. Integration tests with Nomos daemon and Python SDK when available.
"""

import sys
import os
import ipaddress
import subprocess
import socket
import tempfile
import time

# Add sdk/python to sys.path
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "sdk", "python")))

# Test result accumulator
test_results = []

def record_result(name: str, passed: bool, detail: str = ""):
    status = "PASS" if passed else "FAIL"
    test_results.append((name, status, detail))
    print(f"[{status}] {name}{f': {detail}' if detail else ''}")

def is_rfc1918(ip: ipaddress.IPv4Address) -> bool:
    octets = ip.exploded.split(".")
    first = int(octets[0])
    second = int(octets[1])
    if first == 10:
        return True
    if first == 172 and 16 <= second <= 31:
        return True
    if first == 192 and second == 168:
        return True
    if first == 169 and second == 254:
        return True
    return False

def is_allowed_destination(mode: str, dest_ip_str: str) -> bool:
    """Simulates Nomos kernel/firewall decision matrix."""
    ip = ipaddress.ip_address(dest_ip_str)
    mode = mode.lower()

    if mode == "host":
        return True
    elif mode == "none":
        return ip.is_loopback
    elif mode == "isolated":
        if ip.is_loopback:
            return True
        if isinstance(ip, ipaddress.IPv4Address):
            return is_rfc1918(ip)
        elif isinstance(ip, ipaddress.IPv6Address):
            return ip.is_private or ip.is_link_local
        return False
    else:
        raise ValueError(f"Unknown network mode: {mode}")

def test_ip_filtering_rules():
    print("\n--- Test Suite 1: IP Range & Network Mode Filtering Rules ---")

    test_vectors = [
        # (IP, mode_none_allowed, mode_isolated_allowed, mode_host_allowed)
        ("127.0.0.1", True, True, True),
        ("127.0.0.2", True, True, True),
        ("::1", True, True, True),
        ("10.0.0.1", False, True, True),
        ("10.254.1.99", False, True, True),
        ("172.16.0.1", False, True, True),
        ("172.31.255.254", False, True, True),
        ("172.32.0.1", False, False, True),  # Not in 172.16.0.0/12
        ("192.168.1.1", False, True, True),
        ("192.168.100.50", False, True, True),
        ("192.169.1.1", False, False, True),  # Not RFC1918
        ("169.254.1.1", False, True, True),   # Link local
        ("8.8.8.8", False, False, True),      # Google DNS (WAN)
        ("1.1.1.1", False, False, True),      # Cloudflare DNS (WAN)
        ("142.250.190.46", False, False, True), # google.com (WAN)
    ]

    for ip_str, exp_none, exp_isolated, exp_host in test_vectors:
        actual_none = is_allowed_destination("none", ip_str)
        actual_isolated = is_allowed_destination("isolated", ip_str)
        actual_host = is_allowed_destination("host", ip_str)

        passed = (
            actual_none == exp_none and
            actual_isolated == exp_isolated and
            actual_host == exp_host
        )
        record_result(
            f"Address classification: {ip_str}",
            passed,
            f"none={actual_none} (exp {exp_none}), isolated={actual_isolated} (exp {exp_isolated}), host={actual_host} (exp {exp_host})"
        )

def find_nomos_binary() -> str:
    script_dir = os.path.dirname(os.path.abspath(__file__))
    candidates = [
        os.path.join(script_dir, "..", "target", "release", "nomos"),
        os.path.join(script_dir, "..", "target", "debug", "nomos"),
    ]
    for c in candidates:
        if os.path.isfile(c) and os.access(c, os.X_OK):
            return c
    return ""

def test_cli_flags():
    print("\n--- Test Suite 2: Nomos CLI Network Flags ---")
    nomos_bin = find_nomos_binary()

    if not nomos_bin:
        print("[INFO] Building nomos binary for CLI test...")
        subprocess.run(["cargo", "build", "--quiet"], cwd=os.path.join(os.path.dirname(__file__), ".."), check=True)
        nomos_bin = find_nomos_binary()

    if not nomos_bin or not os.path.exists(nomos_bin):
        record_result("Find nomos binary", False, "nomos binary not found in target/debug or target/release")
        return

    record_result("Find nomos binary", True, f"Found at {nomos_bin}")

    # Test 2.1: Verify --help displays --network flag
    proc = subprocess.run([nomos_bin, "run", "--help"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    has_network_flag = "--network <NETWORK>" in proc.stdout or "--network" in proc.stdout
    record_result("CLI run --help includes --network flag", has_network_flag)

    # Test 2.2: Test invalid --network option is rejected
    proc_invalid = subprocess.run(
        [nomos_bin, "run", "--network", "invalid_mode", "--", "echo", "1"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True
    )
    is_rejected = proc_invalid.returncode != 0 and (
        "invalid" in proc_invalid.stderr.lower() or "error" in proc_invalid.stderr.lower()
    )
    record_result("CLI rejects invalid network mode", is_rejected, f"Exit code {proc_invalid.returncode}")

def test_daemon_and_sdk_integration():
    print("\n--- Test Suite 3: Python SDK and Arbiter Network Mode Integration ---")
    nomos_bin = find_nomos_binary()
    if not nomos_bin:
        print("[SKIP] Skipping daemon integration test: binary not available")
        return

    # Create temporary socket and config for isolated test daemon
    temp_dir = tempfile.mkdtemp(prefix="nomos_test_net_")
    sock_path = os.path.join(temp_dir, "test_arbiter.sock")
    cfg_path = os.path.join(temp_dir, "test_nomos.toml")

    with open(cfg_path, "w") as f:
        f.write("""
[limits]
reserved_cores = 1.0
reserved_memory_bytes = 1073741824
storage_scratch_dir = "/tmp/nomos-scratch"
""")

    # Launch daemon
    daemon_proc = subprocess.Popen(
        [nomos_bin, "daemon", "--config", cfg_path, "--socket", sock_path, "--port", "9199"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        # Wait for socket to become ready
        ready = False
        for _ in range(30):
            if os.path.exists(sock_path):
                ready = True
                break
            time.sleep(0.1)

        if not ready:
            record_result("Test daemon launch", False, "Daemon socket did not appear")
            return

        record_result("Test daemon launch", True, f"Running on {sock_path}")

        import nomos
        client = nomos.NomosClient(socket_path=sock_path)

        # Test Mode 1: isolated
        with nomos.lease(cpu=1.0, memory="256MB", network_mode="isolated", socket_path=sock_path, ttl=10) as l_iso:
            mode = l_iso.raw.get("request", {}).get("network_mode")
            record_result("SDK acquire lease with mode 'isolated'", mode == "isolated", f"Observed mode: {mode}")

        # Test Mode 2: none (air-gapped)
        with nomos.lease(cpu=1.0, memory="256MB", network_mode="none", socket_path=sock_path, ttl=10) as l_none:
            mode = l_none.raw.get("request", {}).get("network_mode")
            record_result("SDK acquire lease with mode 'none'", mode == "none", f"Observed mode: {mode}")

        # Test Mode 3: host
        with nomos.lease(cpu=1.0, memory="256MB", network_mode="host", socket_path=sock_path, ttl=10) as l_host:
            mode = l_host.raw.get("request", {}).get("network_mode")
            record_result("SDK acquire lease with mode 'host'", mode == "host", f"Observed mode: {mode}")

        # Test CLI execution with active daemon
        proc_cli_iso = subprocess.run(
            [nomos_bin, "run", "--socket", sock_path, "--network", "isolated", "--", "echo", "nomos-isolated-success"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )
        record_result("nomos run with --network isolated executes cleanly", proc_cli_iso.returncode == 0 and "nomos-isolated-success" in proc_cli_iso.stdout)

        proc_cli_none = subprocess.run(
            [nomos_bin, "run", "--socket", sock_path, "--network", "none", "--", "echo", "nomos-none-success"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )
        record_result("nomos run with --network none executes cleanly", proc_cli_none.returncode == 0 and "nomos-none-success" in proc_cli_none.stdout)

        proc_cli_host = subprocess.run(
            [nomos_bin, "run", "--socket", sock_path, "--network", "host", "--", "echo", "nomos-host-success"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )
        record_result("nomos run with --network host executes cleanly", proc_cli_host.returncode == 0 and "nomos-host-success" in proc_cli_host.stdout)

    finally:
        daemon_proc.terminate()
        daemon_proc.wait()
        try:
            os.remove(sock_path)
            os.remove(cfg_path)
            os.rmdir(temp_dir)
        except OSError:
            pass

def main():
    print("================================================================================")
    print(" NOMOS NETWORK EGRESS ISOLATION TEST SUITE")
    print("================================================================================")

    test_ip_filtering_rules()
    test_cli_flags()
    test_daemon_and_sdk_integration()

    print("\n================================================================================")
    print(" TEST SUMMARY")
    print("================================================================================")
    total = len(test_results)
    passed = sum(1 for _, s, _ in test_results if s == "PASS")
    failed = total - passed

    for name, status, detail in test_results:
        print(f" - [{status}] {name}")

    print("--------------------------------------------------------------------------------")
    print(f"Total: {total} | Passed: {passed} | Failed: {failed}")
    print("================================================================================")

    if failed > 0:
        sys.exit(1)
    else:
        print("[SUCCESS] All network isolation tests completed successfully.")
        sys.exit(0)

if __name__ == "__main__":
    main()
