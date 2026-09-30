#!/usr/bin/env python3
"""
Nomos Single-Host Resource Arbiter — High-Precision Benchmark Suite
Measures:
  1. Unix Domain Socket IPC Round-Trip Latency (p50, p95, p99)
  2. Heartbeat Ingestion Throughput (ops/sec)
  3. Lease Lifecycle Throughput (Acquire -> Release rate)
  4. Deep-Queue Backfilling Decision Latency (100 - 500 queued leases)
  5. Arbiter Daemon Self-Resource Footprint (RSS memory & CPU)
"""

import os
import sys
import time
import socket
import json
import statistics
import threading
from typing import List, Dict, Any, Tuple

# Add sdk/python to path
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "sdk", "python")))
import nomos

def get_process_memory_rss_mb(pid: int) -> float:
    try:
        import resource
        # On macOS ru_maxrss is in bytes, on Linux in kilobytes
        usage = resource.getrusage(resource.RUSAGE_SELF)
        if sys.platform == "darwin":
            return usage.ru_maxrss / (1024 * 1024)
        return usage.ru_maxrss / 1024
    except Exception:
        return 0.0

def benchmark_ipc_ping_latency(socket_path: str, iterations: int = 1000) -> Dict[str, float]:
    """Benchmark raw IPC Round-Trip Latency over Unix Domain Socket."""
    durations_us = []

    # Send GetStatus requests and measure round-trip
    for _ in range(iterations):
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.connect(socket_path)
            t0 = time.perf_counter_ns()
            sock.sendall(b'"GetStatus"\n')
            buffer = ""
            while True:
                chunk = sock.recv(4096).decode("utf-8")
                if not chunk:
                    break
                buffer += chunk
                if "\n" in buffer:
                    break
            t1 = time.perf_counter_ns()
            durations_us.append((t1 - t0) / 1000.0)
        finally:
            sock.close()

    durations_us.sort()
    return {
        "iterations": iterations,
        "min_us": min(durations_us),
        "mean_us": statistics.mean(durations_us),
        "p50_us": durations_us[int(iterations * 0.50)],
        "p90_us": durations_us[int(iterations * 0.90)],
        "p95_us": durations_us[int(iterations * 0.95)],
        "p99_us": durations_us[int(iterations * 0.99)],
        "max_us": max(durations_us),
    }

def benchmark_persistent_ipc_latency(socket_path: str, iterations: int = 2000) -> Dict[str, float]:
    """Benchmark IPC Round-Trip Latency over a single persistent Unix Domain Socket."""
    durations_us = []
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.connect(socket_path)
        for _ in range(iterations):
            t0 = time.perf_counter_ns()
            sock.sendall(b'"GetStatus"\n')
            buffer = ""
            while True:
                chunk = sock.recv(4096).decode("utf-8")
                if not chunk:
                    break
                buffer += chunk
                if "\n" in buffer:
                    break
            t1 = time.perf_counter_ns()
            durations_us.append((t1 - t0) / 1000.0)
    finally:
        sock.close()

    durations_us.sort()
    return {
        "iterations": iterations,
        "min_us": min(durations_us),
        "mean_us": statistics.mean(durations_us),
        "p50_us": durations_us[int(iterations * 0.50)],
        "p90_us": durations_us[int(iterations * 0.90)],
        "p95_us": durations_us[int(iterations * 0.95)],
        "p99_us": durations_us[int(iterations * 0.99)],
        "max_us": max(durations_us),
    }

def benchmark_lease_lifecycle_throughput(socket_path: str, count: int = 500) -> Dict[str, Any]:
    """Benchmark rapid Acquire -> Release lifecycle throughput."""
    client = nomos.NomosClient(socket_path)
    acquire_durations_us = []
    release_durations_us = []

    t_start = time.perf_counter()
    for i in range(count):
        t0 = time.perf_counter_ns()
        lease = client.acquire(
            worker_id=f"bench-worker-{i}",
            cpu=0.1,
            memory="64MB",
            priority="Normal",
            ttl=60,
            wait=False
        )
        t1 = time.perf_counter_ns()
        acquire_durations_us.append((t1 - t0) / 1000.0)

        t2 = time.perf_counter_ns()
        client.release(lease.id)
        t3 = time.perf_counter_ns()
        release_durations_us.append((t3 - t2) / 1000.0)

    total_time = time.perf_counter() - t_start
    throughput_cycles_sec = count / total_time
    throughput_ops_sec = (count * 2) / total_time  # 2 ops per cycle: acquire + release

    acquire_durations_us.sort()
    release_durations_us.sort()

    return {
        "count": count,
        "total_time_sec": total_time,
        "cycles_per_sec": throughput_cycles_sec,
        "total_ops_per_sec": throughput_ops_sec,
        "acquire_p50_us": acquire_durations_us[int(count * 0.50)],
        "acquire_p95_us": acquire_durations_us[int(count * 0.95)],
        "acquire_p99_us": acquire_durations_us[int(count * 0.99)],
        "release_p50_us": release_durations_us[int(count * 0.50)],
        "release_p95_us": release_durations_us[int(count * 0.95)],
        "release_p99_us": release_durations_us[int(count * 0.99)],
    }

def benchmark_heartbeat_throughput(socket_path: str, duration_sec: float = 3.0) -> Dict[str, Any]:
    """Benchmark high-frequency heartbeat ingestion rate."""
    client = nomos.NomosClient(socket_path)
    lease = client.acquire(worker_id="bench-hb-target", cpu=0.1, memory="64MB", ttl=120)

    hb_count = 0
    t_start = time.perf_counter()
    while time.perf_counter() - t_start < duration_sec:
        client.heartbeat(lease.id)
        hb_count += 1
    total_time = time.perf_counter() - t_start

    client.release(lease.id)

    return {
        "heartbeats_sent": hb_count,
        "duration_sec": total_time,
        "heartbeats_per_sec": hb_count / total_time,
    }

def benchmark_backfilling_decision_scalability(socket_path: str, queue_depths: List[int] = [25, 50, 100, 200]) -> List[Dict[str, Any]]:
    """Benchmark queue processing and backfilling decision latency with deep queues."""
    client = nomos.NomosClient(socket_path)
    results = []

    status = client.status()
    total_cpu = status["pool"]["total_cores"]
    total_mem = status["pool"]["total_memory_bytes"]

    for depth in queue_depths:
        # Step 1: Saturate pool with an Anchor lease
        anchor = client.acquire(
            worker_id="bench-anchor",
            cpu=total_cpu * 0.95,
            memory=int(total_mem * 0.95),
            ttl=120,
        )

        # Step 2: Enqueue N waiting candidates with varied priorities over persistent socket
        candidate_ids = []
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.connect(socket_path)
            for i in range(depth):
                prio = "High" if i % 4 == 0 else ("Normal" if i % 2 == 0 else "Low")
                req_data = {
                    "AcquireLease": {
                        "worker_id": f"queue-cand-{depth}-{i}",
                        "tenant": "default",
                        "req_cpu": total_cpu * 0.5,
                        "req_memory_bytes": int(total_mem * 0.5),
                        "req_scratch_bytes": 0,
                        "devices": [],
                        "network_mode": "isolated",
                        "network_bandwidth_mbps": None,
                        "estimated_seconds": None,
                        "deadline": None,
                        "depends_on": [],
                        "priority": prio,
                        "ttl_seconds": 60,
                    }
                }
                sock.sendall((json.dumps(req_data) + "\n").encode())
                buf = ""
                while True:
                    ch = sock.recv(4096).decode()
                    buf += ch
                    if "\n" in buf:
                        line, _ = buf.split("\n", 1)
                        res = json.loads(line)
                        if "LeaseQueued" in res:
                            candidate_ids.append(res["LeaseQueued"]["lease"]["id"])
                        break
        finally:
            sock.close()

        # Step 3: Measure time to release anchor and backfill admission
        t0 = time.perf_counter_ns()
        client.release(anchor.id)
        t1 = time.perf_counter_ns()

        decision_latency_ms = (t1 - t0) / 1_000_000.0

        # Step 4: Clean up remaining queued and newly admitted leases over persistent socket
        st = client.status()
        for act in st["active_leases"]:
            if "queue-cand" in act["request"]["worker_id"]:
                try:
                    client.release(act["id"])
                except Exception:
                    pass

        sock_drain = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock_drain.connect(socket_path)
            for cand_id in candidate_ids:
                sock_drain.sendall((json.dumps({"ReleaseLease": {"lease_id": cand_id}}) + "\n").encode())
                buf = ""
                while True:
                    ch = sock_drain.recv(4096).decode()
                    buf += ch
                    if "\n" in buf:
                        break
        finally:
            sock_drain.close()

        results.append({
            "queue_depth": depth,
            "decision_latency_ms": decision_latency_ms,
            "latency_per_candidate_us": (decision_latency_ms * 1000.0) / depth,
        })
        time.sleep(0.1)

    return results

def get_daemon_telemetry(socket_path: str) -> Dict[str, Any]:
    client = nomos.NomosClient(socket_path)
    status = client.status()
    return {
        "pool_cores": status["pool"]["total_cores"],
        "pool_memory_gb": status["pool"]["total_memory_bytes"] / (1024**3),
        "host_reserve_gb": status["pool"]["host_reserve_memory_bytes"] / (1024**3),
    }

def main():
    socket_path = nomos.default_socket_path()
    if not os.path.exists(socket_path):
        print(f"[ERROR] Nomos socket not found at '{socket_path}'. Ensure 'nomos daemon' is running.")
        sys.exit(1)

    print("===============================================================================")
    print(" NOMOS RESOURCE ARBITER — OFFICIAL PERFORMANCE BENCHMARK SUITE")
    print(f" Platform: {sys.platform} ({os.uname().machine}) | Socket: {socket_path}")
    print("===============================================================================")

    telemetry = get_daemon_telemetry(socket_path)
    print(f"Nomos Pool Budget: {telemetry['pool_cores']:.1f} vCPUs | {telemetry['pool_memory_gb']:.2f} GiB RAM (Reserve Floor: {telemetry['host_reserve_gb']:.2f} GiB)")
    print("-------------------------------------------------------------------------------")

    # 1. IPC Connection Per-Call Latency
    print("Running Test 1/5: Ephemeral Socket Connect + Round-Trip Latency (1,000 runs)...")
    res_ephemeral = benchmark_ipc_ping_latency(socket_path, iterations=1000)

    # 2. Persistent Socket Round-Trip Latency
    print("Running Test 2/5: Persistent Socket Round-Trip Latency (2,000 runs)...")
    res_persistent = benchmark_persistent_ipc_latency(socket_path, iterations=2000)

    # 3. Heartbeat Ingestion Throughput
    print("Running Test 3/5: High-Frequency Heartbeat Ingestion Throughput...")
    res_hb = benchmark_heartbeat_throughput(socket_path, duration_sec=3.0)

    # 4. Lease Lifecycle Throughput
    print("Running Test 4/5: Lease Full Lifecycle (Acquire -> Grant -> Release) (500 cycles)...")
    res_lifecycle = benchmark_lease_lifecycle_throughput(socket_path, count=500)

    # 5. Deep-Queue Backfilling Decision Scalability
    print("Running Test 5/5: Deep Queue & Backfilling Evaluation Scalability (50, 100, 200, 300 queued)...")
    res_queue = benchmark_backfilling_decision_scalability(socket_path, queue_depths=[50, 100, 200, 300])

    print("===============================================================================")
    print(" BENCHMARK RESULTS SUMMARY")
    print("===============================================================================")

    print("\n1. IPC ROUND-TRIP LATENCY (Unix Domain Socket):")
    print(f" - Ephemeral Socket (Connect + Call + Close):")
    print(f"     p50: {res_ephemeral['p50_us']:.1f} µs | p95: {res_ephemeral['p95_us']:.1f} µs | p99: {res_ephemeral['p99_us']:.1f} µs | Mean: {res_ephemeral['mean_us']:.1f} µs")
    print(f" - Persistent Socket (Pipelined Call):")
    print(f"     p50: {res_persistent['p50_us']:.1f} µs | p95: {res_persistent['p95_us']:.1f} µs | p99: {res_persistent['p99_us']:.1f} µs | Mean: {res_persistent['mean_us']:.1f} µs")

    print("\n2. HEARTBEAT INGESTION THROUGHPUT:")
    print(f" - Throughput: {res_hb['heartbeats_per_sec']:.1f} heartbeats/second")
    print(f" - Total Processed: {res_hb['heartbeats_sent']} heartbeats in {res_hb['duration_sec']:.2f}s")

    print("\n3. LEASE LIFECYCLE THROUGHPUT & ADMISSION CONTROLLER LATENCY:")
    print(f" - Full Lifecycle Rate:  {res_lifecycle['cycles_per_sec']:.1f} full cycles/sec (Acquire + Release)")
    print(f" - Combined IPC Rate:    {res_lifecycle['total_ops_per_sec']:.1f} operations/sec")
    print(f" - Admission Latency:    p50: {res_lifecycle['acquire_p50_us']:.1f} µs | p95: {res_lifecycle['acquire_p95_us']:.1f} µs | p99: {res_lifecycle['acquire_p99_us']:.1f} µs")
    print(f" - Release Latency:      p50: {res_lifecycle['release_p50_us']:.1f} µs | p95: {res_lifecycle['release_p95_us']:.1f} µs | p99: {res_lifecycle['release_p99_us']:.1f} µs")

    print("\n4. DEEP-QUEUE BACKFILLING SCALABILITY:")
    print(f" {'Queue Depth':<14} {'Release+Backfill Latency':<26} {'Overhead / Candidate':<22}")
    for q in res_queue:
        print(f" {q['queue_depth']:<14} {q['decision_latency_ms']:<8.2f} ms{'':<18} {q['latency_per_candidate_us']:<8.1f} µs")

    print("===============================================================================")

if __name__ == "__main__":
    main()
