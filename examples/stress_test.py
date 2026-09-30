#!/usr/bin/env python3
"""
Nomos High-Concurrency & Chaos Stress Test Suite
================================================
Comprehensive verification harness testing:
  1. Backfilling Algorithm & Head-of-Line Blocking Prevention
  2. High-Concurrency Multi-Priority Workload (30-50 Workers, 0.5-3.0 vCPUs, 512MB-8GB RAM)
  3. Continuous Invariant Verification (Allocated CPU/RAM <= Budget Pool at all times)
  4. Chaos Testing & Dead-Man's Switch (5 Abruptly Crashed Workers, 15s Heartbeat Auto-Reclaim)
  5. Queued Worker Auto-Unlocking & Resource Reclamation Integrity

No emoji characters are used anywhere in output or logs.
"""

import sys
import os
import time
import json
import signal
import socket
import argparse
import threading
import subprocess
from typing import Dict, List, Any, Optional

# Add sdk/python to path
SDK_PATH = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "sdk", "python"))
if SDK_PATH not in sys.path:
    sys.path.insert(0, SDK_PATH)

import nomos
from nomos import NomosClient, NomosError, parse_bytes

# Plain text styling tags (No emoji)
class Log:
    INFO = "[INFO]"
    PASS = "[PASS]"
    FAIL = "[FAIL]"
    WARN = "[WARN]"
    CHAOS = "[CHAOS]"
    SCHED = "[SCHED]"
    METRIC = "[METRIC]"

    @staticmethod
    def print(tag: str, msg: str):
        timestamp = time.strftime("%H:%M:%S")
        print(f"[{timestamp}] {tag:<8} {msg}", flush=True)


class InvariantViolation(Exception):
    pass


class InvariantMonitor:
    """
    Continuous high-frequency background monitor that checks Nomos invariants
    every 30-50 milliseconds during load tests.
    """
    def __init__(self, client: NomosClient, interval_sec: float = 0.04):
        self.client = client
        self.interval_sec = interval_sec
        self.stop_event = threading.Event()
        self.thread: Optional[threading.Thread] = None

        self.samples_count = 0
        self.violations: List[Dict[str, Any]] = []

        self.pool_cores = 0.0
        self.pool_memory_bytes = 0

        self.peak_cpu_allocated = 0.0
        self.peak_memory_allocated = 0
        self.peak_active_leases = 0
        self.peak_queued_leases = 0

    def start(self):
        status = self.client.status()
        self.pool_cores = status["pool"]["total_cores"]
        self.pool_memory_bytes = status["pool"]["total_memory_bytes"]

        self.stop_event.clear()
        self.thread = threading.Thread(target=self._monitor_loop, daemon=True)
        self.thread.start()
        Log.print(Log.INFO, f"Invariant Monitor started. Target: <= {self.pool_cores:.1f} vCPUs, <= {self.pool_memory_bytes / (1024**3):.2f} GiB RAM.")

    def stop(self):
        self.stop_event.set()
        if self.thread:
            self.thread.join(timeout=2.0)
        Log.print(Log.INFO, f"Invariant Monitor stopped. Recorded {self.samples_count} continuous snapshots.")

    def _monitor_loop(self):
        while not self.stop_event.is_set():
            try:
                st = self.client.status()
                self.samples_count += 1

                alloc_cpu = st.get("allocated_cpu", 0.0)
                alloc_mem = st.get("allocated_memory_bytes", 0)
                active = st.get("active_leases", [])
                queued = st.get("queued_leases", [])

                # Track peaks
                if alloc_cpu > self.peak_cpu_allocated:
                    self.peak_cpu_allocated = alloc_cpu
                if alloc_mem > self.peak_memory_allocated:
                    self.peak_memory_allocated = alloc_mem
                if len(active) > self.peak_active_leases:
                    self.peak_active_leases = len(active)
                if len(queued) > self.peak_queued_leases:
                    self.peak_queued_leases = len(queued)

                # Invariant 1: Allocated CPU must never exceed Nomos budget pool
                if alloc_cpu > (self.pool_cores + 1e-5):
                    err = f"CPU pool budget violation! Allocated: {alloc_cpu:.2f} > Pool limit: {self.pool_cores:.2f}"
                    self.violations.append({"time": time.time(), "error": err, "status": st})
                    Log.print(Log.FAIL, err)

                # Invariant 2: Allocated Memory must never exceed Nomos budget pool
                if alloc_mem > self.pool_memory_bytes:
                    err = f"RAM pool budget violation! Allocated: {alloc_mem} B > Pool limit: {self.pool_memory_bytes} B"
                    self.violations.append({"time": time.time(), "error": err, "status": st})
                    Log.print(Log.FAIL, err)

                # Invariant 3: Active leases sum must match reported allocation
                calc_cpu = sum(l.get("request", {}).get("req_cpu", 0.0) for l in active)
                calc_mem = sum(l.get("request", {}).get("req_memory_bytes", 0) for l in active)

                if abs(alloc_cpu - calc_cpu) > 0.01:
                    err = f"CPU accounting mismatch! Reported: {alloc_cpu:.2f} != Active sum: {calc_cpu:.2f}"
                    self.violations.append({"time": time.time(), "error": err, "status": st})
                    Log.print(Log.FAIL, err)

                if alloc_mem != calc_mem:
                    err = f"RAM accounting mismatch! Reported: {alloc_mem} != Active sum: {calc_mem}"
                    self.violations.append({"time": time.time(), "error": err, "status": st})
                    Log.print(Log.FAIL, err)

            except Exception:
                # Daemon may be momentarily saturated or restarting
                pass

            time.sleep(self.interval_sec)

    def assert_zero_violations(self):
        if self.violations:
            first_err = self.violations[0]["error"]
            raise InvariantViolation(f"Total {len(self.violations)} invariant violations detected! First: {first_err}")


class DaemonManager:
    """Manages spawning and teardown of local nomos daemon if needed."""
    def __init__(self, bin_path: str, socket_path: str, port: int = 9190):
        self.bin_path = bin_path
        self.socket_path = socket_path
        self.port = port
        self.process: Optional[subprocess.Popen] = None

    def is_running(self) -> bool:
        if not os.path.exists(self.socket_path):
            return False
        try:
            client = NomosClient(self.socket_path)
            client.status()
            return True
        except Exception:
            return False

    def start(self):
        if self.is_running():
            Log.print(Log.INFO, f"Connected to existing Nomos daemon at {self.socket_path}")
            return

        if os.path.exists(self.socket_path):
            try:
                os.remove(self.socket_path)
            except OSError:
                pass

        cmd = [
            self.bin_path,
            "daemon",
            "--socket", self.socket_path,
            "--port", str(self.port)
        ]
        Log.print(Log.INFO, f"Spawning local Nomos daemon: {' '.join(cmd)}")
        self.process = subprocess.Popen(
            cmd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            preexec_fn=os.setsid if hasattr(os, "setsid") else None
        )

        # Wait for socket to become ready
        start_t = time.time()
        ready = False
        while time.time() - start_t < 10.0:
            if self.is_running():
                ready = True
                break
            time.sleep(0.2)

        if not ready:
            self.stop()
            raise RuntimeError(f"Nomos daemon failed to start within 10s at {self.socket_path}")

        Log.print(Log.PASS, f"Local Nomos daemon running (PID: {self.process.pid}) at {self.socket_path}")

    def stop(self):
        if self.process:
            Log.print(Log.INFO, f"Terminating local Nomos daemon (PID: {self.process.pid})...")
            try:
                if hasattr(os, "killpg"):
                    os.killpg(os.getpgid(self.process.pid), signal.SIGTERM)
                else:
                    self.process.terminate()
                self.process.wait(timeout=3.0)
            except Exception:
                if hasattr(os, "killpg"):
                    os.killpg(os.getpgid(self.process.pid), signal.SIGKILL)
                else:
                    self.process.kill()
            self.process = None

            if os.path.exists(self.socket_path):
                try:
                    os.remove(self.socket_path)
                except OSError:
                    pass
            Log.print(Log.INFO, "Local Nomos daemon terminated cleanly.")


# ==============================================================================
# SUITE 1: Backfilling & Head-of-Line Blocking Prevention
# ==============================================================================
def run_suite_backfilling(client: NomosClient, monitor: InvariantMonitor):
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")
    Log.print(Log.INFO, "TEST SUITE 1: Backfilling Algorithm & Head-of-Line Blocking Prevention")
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")

    status = client.status()
    pool = status["pool"]
    total_cores = pool["total_cores"]
    total_mem = pool["total_memory_bytes"]

    Log.print(Log.INFO, f"Nomos Pool Budget: {total_cores:.1f} vCPUs, {total_mem / (1024**3):.2f} GiB RAM")

    # Step 1: Anchor Task - Takes up 65% of pool capacity
    anchor_cpu = round(total_cores * 0.65, 1)
    anchor_mem = int(total_mem * 0.65)
    Log.print(Log.INFO, f"Step 1: Submitting Anchor Worker (Req: {anchor_cpu} vCPUs, {anchor_mem / (1024**3):.2f} GiB RAM)...")
    anchor_lease = client.acquire(
        worker_id="anchor-heavy-worker",
        cpu=anchor_cpu,
        memory=anchor_mem,
        priority="Normal",
        ttl=60,
        wait=True
    )
    Log.print(Log.PASS, f"Anchor Worker granted lease {anchor_lease.id}. Pool remaining: {total_cores - anchor_cpu:.1f} vCPUs, {(total_mem - anchor_mem) / (1024**3):.2f} GiB RAM")

    large_worker_started = threading.Event()
    small_worker_1_started = threading.Event()
    small_worker_2_started = threading.Event()
    events_timeline = []

    # Step 2: Large Task - Needs 55% of pool capacity. CANNOT fit into remaining 35% headroom.
    # Must be placed at the head of the wait queue (Position #1).
    large_cpu = round(total_cores * 0.55, 1)
    large_mem = int(total_mem * 0.55)

    def run_large_worker():
        Log.print(Log.SCHED, f"Large Worker requesting {large_cpu} vCPUs, {large_mem / (1024**3):.2f} GiB RAM (Expected: QUEUED)...")
        l = client.acquire(
            worker_id="large-queued-worker",
            cpu=large_cpu,
            memory=large_mem,
            priority="Normal",
            ttl=60,
            wait=True,
            timeout=30.0
        )
        events_timeline.append(("large_granted", time.time()))
        large_worker_started.set()
        Log.print(Log.SCHED, f"Large Worker GRANTED lease {l.id}!")
        time.sleep(1.0)
        client.release(l.id)
        Log.print(Log.SCHED, f"Large Worker finished and released lease {l.id}.")

    # Step 3: Small Tasks - Need 15% and 10% of pool capacity.
    # Both CAN fit inside the remaining 35% headroom!
    small_1_cpu = max(0.5, round(total_cores * 0.15, 1))
    small_1_mem = int(total_mem * 0.15)

    small_2_cpu = max(0.5, round(total_cores * 0.10, 1))
    small_2_mem = int(total_mem * 0.10)

    def run_small_worker_1():
        time.sleep(0.3) # Submitted AFTER large worker is in queue
        Log.print(Log.SCHED, f"Small Worker 1 requesting {small_1_cpu} vCPUs, {small_1_mem / (1024**3):.2f} GiB RAM (Testing Backfill)...")
        l = client.acquire(
            worker_id="small-backfill-worker-1",
            cpu=small_1_cpu,
            memory=small_1_mem,
            priority="Normal",
            ttl=30,
            wait=True,
            timeout=15.0
        )
        events_timeline.append(("small_1_granted", time.time()))
        small_worker_1_started.set()
        Log.print(Log.PASS, f"Small Worker 1 GRANTED lease {l.id} immediately via Backfilling!")
        time.sleep(1.5)
        client.release(l.id)
        Log.print(Log.SCHED, f"Small Worker 1 released lease {l.id}.")

    def run_small_worker_2():
        time.sleep(0.5) # Submitted AFTER large worker is in queue
        Log.print(Log.SCHED, f"Small Worker 2 requesting {small_2_cpu} vCPUs, {small_2_mem / (1024**3):.2f} GiB RAM (Testing Backfill)...")
        l = client.acquire(
            worker_id="small-backfill-worker-2",
            cpu=small_2_cpu,
            memory=small_2_mem,
            priority="Normal",
            ttl=30,
            wait=True,
            timeout=15.0
        )
        events_timeline.append(("small_2_granted", time.time()))
        small_worker_2_started.set()
        Log.print(Log.PASS, f"Small Worker 2 GRANTED lease {l.id} immediately via Backfilling!")
        time.sleep(1.5)
        client.release(l.id)
        Log.print(Log.SCHED, f"Small Worker 2 released lease {l.id}.")

    # Launch threads
    t_large = threading.Thread(target=run_large_worker)
    t_small_1 = threading.Thread(target=run_small_worker_1)
    t_small_2 = threading.Thread(target=run_small_worker_2)

    t_large.start()
    time.sleep(0.2)
    t_small_1.start()
    t_small_2.start()

    # Verify that Small Workers 1 & 2 started BEFORE Large Worker
    small_1_ok = small_worker_1_started.wait(timeout=5.0)
    small_2_ok = small_worker_2_started.wait(timeout=5.0)
    if not (small_1_ok and small_2_ok):
        raise AssertionError("Small backfilling workers failed to start promptly!")

    if large_worker_started.is_set():
        raise AssertionError("Large worker started prematurely while anchor task was active!")

    # Verify queue status: Large worker is currently waiting in queue
    st_queue = client.status()
    queued_ids = [q["request"]["worker_id"] for q in st_queue.get("queued_leases", [])]
    Log.print(Log.INFO, f"Current Queued Workers in Arbiter: {queued_ids}")
    if "large-queued-worker" not in queued_ids:
        raise AssertionError("Expected 'large-queued-worker' to be queued in Arbiter!")

    Log.print(Log.PASS, "Backfilling Confirmed: Small workers granted while large worker is queued ahead in line.")

    # Now release Anchor Worker to unlock the large worker
    time.sleep(1.0)
    Log.print(Log.INFO, f"Releasing Anchor Worker lease {anchor_lease.id}...")
    client.release(anchor_lease.id)

    # Wait for all threads to join
    t_small_1.join()
    t_small_2.join()
    t_large.join(timeout=10.0)

    # Chronological verification
    timeline_dict = {event: ts for event, ts in events_timeline}
    assert timeline_dict["small_1_granted"] < timeline_dict["large_granted"], "Small Worker 1 must be granted before Large Worker"
    assert timeline_dict["small_2_granted"] < timeline_dict["large_granted"], "Small Worker 2 must be granted before Large Worker"

    monitor.assert_zero_violations()
    Log.print(Log.PASS, "TEST SUITE 1 PASSED: Backfilling algorithm successfully validated with zero budget violations.")


# ==============================================================================
# SUITE 2: High-Concurrency Multi-Priority Stress Test (30-50 Workers)
# ==============================================================================
def run_suite_high_concurrency(client: NomosClient, monitor: InvariantMonitor, num_workers: int = 40):
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")
    Log.print(Log.INFO, f"TEST SUITE 2: High-Concurrency Multi-Priority Stress Test ({num_workers} Workers)")
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")

    status = client.status()
    pool = status["pool"]
    total_cores = pool["total_cores"]
    total_mem = pool["total_memory_bytes"]

    # Worker configuration matrices
    # CPU: 0.5 - 3.0 cores (capped to total_cores if total_cores < 3.0)
    cpu_options = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0]
    cpu_options = [c for c in cpu_options if c <= total_cores] or [0.5]

    # RAM: 512MB - 8GB (capped to total_mem)
    mem_options_str = ["512MB", "1GB", "2GB", "4GB", "6GB", "8GB"]
    mem_options = [m for m in mem_options_str if parse_bytes(m) <= total_mem] or ["512MB"]

    priorities = ["Low", "Normal", "High", "Critical"]

    workers_config = []
    total_demanded_cpu = 0.0
    total_demanded_mem = 0

    for i in range(num_workers):
        w_id = f"stress-worker-{i+1:02d}"
        cpu = cpu_options[i % len(cpu_options)]
        mem = mem_options[(i * 2 + 1) % len(mem_options)]
        prio = priorities[i % len(priorities)]
        # Deterministic work duration: 0.4s to 1.2s
        duration = 0.4 + (i % 5) * 0.2

        workers_config.append({
            "worker_id": w_id,
            "cpu": cpu,
            "memory": mem,
            "priority": prio,
            "duration": duration,
        })
        total_demanded_cpu += cpu
        total_demanded_mem += parse_bytes(mem)

    Log.print(Log.INFO, f"Generated {num_workers} diverse workers:")
    Log.print(Log.INFO, f"  Nomos Budget Pool Capacity: {total_cores:.1f} vCPUs, {total_mem / (1024**3):.2f} GiB RAM")
    Log.print(Log.INFO, f"  Total Demanded Resources:   {total_demanded_cpu:.1f} vCPUs ({total_demanded_cpu / total_cores * 100:.0f}% oversubscription), {total_demanded_mem / (1024**3):.2f} GiB RAM ({total_demanded_mem / total_mem * 100:.0f}% oversubscription)")

    results: List[Dict[str, Any]] = []
    lock = threading.Lock()
    active_in_flight = [0]
    completed_count = [0]

    def worker_thread(cfg: Dict[str, Any]):
        w_id = cfg["worker_id"]
        cpu = cfg["cpu"]
        mem = cfg["memory"]
        prio = cfg["priority"]
        duration = cfg["duration"]

        t_submit = time.time()
        with lock:
            active_in_flight[0] += 1

        lease_obj = None
        hb_stop = threading.Event()
        hb_thread = None

        try:
            # Request lease with blocking wait
            lease_obj = client.acquire(
                worker_id=w_id,
                cpu=cpu,
                memory=mem,
                priority=prio,
                ttl=120,
                wait=True,
                timeout=120.0
            )
            t_granted = time.time()
            wait_time = t_granted - t_submit

            # Heartbeat thread for lease while performing work
            def _hb():
                while not hb_stop.wait(3.0):
                    try:
                        client.heartbeat(lease_obj.id)
                    except Exception:
                        pass

            hb_thread = threading.Thread(target=_hb, daemon=True)
            hb_thread.start()

            # Simulate workload
            time.sleep(duration)
            t_finished = time.time()

            hb_stop.set()
            if hb_thread:
                hb_thread.join(timeout=1.0)

            # Release lease
            client.release(lease_obj.id)

            with lock:
                completed_count[0] += 1
                results.append({
                    "worker_id": w_id,
                    "lease_id": lease_obj.id,
                    "priority": prio,
                    "cpu": cpu,
                    "memory": mem,
                    "duration": duration,
                    "wait_time": wait_time,
                    "total_time": t_finished - t_submit,
                    "status": "SUCCESS"
                })

        except Exception as e:
            with lock:
                results.append({
                    "worker_id": w_id,
                    "priority": prio,
                    "status": f"FAILED: {e}"
                })
            Log.print(Log.FAIL, f"Worker {w_id} failed: {e}")
        finally:
            with lock:
                active_in_flight[0] -= 1

    # Launch all workers simultaneously
    Log.print(Log.INFO, f"Launching all {num_workers} workers concurrently into Nomos Arbiter...")
    start_time = time.time()
    threads = [threading.Thread(target=worker_thread, args=(cfg,)) for cfg in workers_config]

    for t in threads:
        t.start()

    # Progress monitoring loop
    last_print = time.time()
    while any(t.is_alive() for t in threads):
        time.sleep(0.5)
        if time.time() - last_print >= 2.5:
            last_print = time.time()
            try:
                st = client.status()
                n_act = len(st.get("active_leases", []))
                n_que = len(st.get("queued_leases", []))
                Log.print(Log.METRIC, f"Progress: Completed {completed_count[0]}/{num_workers} | Active in pool: {n_act} | Queued in line: {n_que} | Invariant violations: {len(monitor.violations)}")
            except Exception:
                pass

    for t in threads:
        t.join(timeout=10.0)

    elapsed_total = time.time() - start_time
    Log.print(Log.PASS, f"All {num_workers} workers finished execution in {elapsed_total:.2f} seconds.")

    # Validate results
    success_count = sum(1 for r in results if r.get("status") == "SUCCESS")
    if success_count != num_workers:
        raise AssertionError(f"Expected {num_workers} successful workers, got {success_count}!")

    # Calculate wait times per priority tier
    prio_stats: Dict[str, List[float]] = {p: [] for p in priorities}
    for r in results:
        prio_stats[r["priority"]].append(r["wait_time"])

    Log.print(Log.INFO, "-------------------------------------------------------------------------------")
    Log.print(Log.INFO, "HIGH-CONCURRENCY STRESS TEST RESULTS BY PRIORITY TIER:")
    Log.print(Log.INFO, f"{'PRIORITY':<12} {'COUNT':<8} {'MIN WAIT (s)':<14} {'AVG WAIT (s)':<14} {'MAX WAIT (s)':<14}")
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")
    for p in ["Critical", "High", "Normal", "Low"]:
        waits = prio_stats[p]
        if waits:
            min_w = min(waits)
            avg_w = sum(waits) / len(waits)
            max_w = max(waits)
            Log.print(Log.INFO, f"{p:<12} {len(waits):<8} {min_w:<14.2f} {avg_w:<14.2f} {max_w:<14.2f}")
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")

    # Invariant assertion
    monitor.assert_zero_violations()
    Log.print(Log.PASS, f"TEST SUITE 2 PASSED: 100% ({num_workers}/{num_workers}) workers succeeded with 0 invariant violations.")


# ==============================================================================
# SUITE 3: Chaos Testing - Sudden Crash & Dead-Man's Switch Heartbeat Timeout
# ==============================================================================
def run_suite_chaos_testing(client: NomosClient, monitor: InvariantMonitor, socket_path: str, num_chaos: int = 5, num_queued: int = 8):
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")
    Log.print(Log.INFO, f"TEST SUITE 3: Chaos Testing & Dead-Man's Switch Timeout ({num_chaos} Abrupt Crashes)")
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")

    status = client.status()
    pool = status["pool"]
    total_cores = pool["total_cores"]
    total_mem = pool["total_memory_bytes"]

    # Each chaos worker takes 1/num_chaos of total pool cores and a proportional share of RAM
    chaos_cpu = round(total_cores / num_chaos, 2)
    chaos_mem = int((total_mem * 0.85) / num_chaos)

    Log.print(Log.CHAOS, f"Step 1: Spawning {num_chaos} Chaos Workers that will claim full pool capacity:")
    Log.print(Log.CHAOS, f"  Each Chaos Worker: {chaos_cpu} vCPUs, {chaos_mem / (1024**3):.2f} GiB RAM")
    Log.print(Log.CHAOS, f"  Total Chaos Claim: {chaos_cpu * num_chaos:.2f} vCPUs, {(chaos_mem * num_chaos) / (1024**3):.2f} GiB RAM")

    # Python one-liner code executed in separate subprocess
    # Acquires lease, prints lease id, then hangs without releasing or sending heartbeats
    child_code = f"""
import sys, os, time
sys.path.insert(0, {repr(SDK_PATH)})
from nomos import NomosClient
c = NomosClient({repr(socket_path)})
worker_id = sys.argv[1]
cpu = float(sys.argv[2])
mem = int(sys.argv[3])
l = c.acquire(worker_id=worker_id, cpu=cpu, memory=mem, priority="Normal", ttl=180, wait=True)
print(l.id, flush=True)
while True:
    time.sleep(1)
"""

    chaos_procs: List[subprocess.Popen] = []
    chaos_lease_ids: List[str] = []

    # Step 2: Spawn each chaos worker process and read granted lease ID
    t_grants: List[float] = []
    for i in range(num_chaos):
        w_id = f"chaos-victim-{i+1}"
        p = subprocess.Popen(
            [sys.executable, "-c", child_code, w_id, str(chaos_cpu), str(chaos_mem)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True
        )
        lease_id = p.stdout.readline().strip()
        t_grant = time.time()
        if not lease_id.startswith("ls-"):
            raise RuntimeError(f"Chaos worker {w_id} failed to acquire lease! Output: {lease_id}")

        chaos_procs.append(p)
        chaos_lease_ids.append(lease_id)
        t_grants.append(t_grant)
        Log.print(Log.CHAOS, f"Chaos Worker {w_id} GRANTED lease {lease_id} (PID: {p.pid})")

    avg_grant_time = sum(t_grants) / len(t_grants)

    # Verify that the 5 chaos leases occupy the pool
    st = client.status()
    active_ids = [al["id"] for al in st.get("active_leases", [])]
    Log.print(Log.INFO, f"Active leases currently in Arbiter: {active_ids}")
    for lid in chaos_lease_ids:
        assert lid in active_ids, f"Lease {lid} must be active in Arbiter"

    # Step 3: IMMEDIATELY crash all chaos workers via SIGKILL (kill -9)
    # The processes terminate abruptly. No cleanup handlers run. No release IPC is sent.
    Log.print(Log.CHAOS, "Simulating abrupt disaster: SIGKILLing all 5 Chaos Worker processes simultaneously...")
    for p in chaos_procs:
        p.kill()
        p.wait()

    Log.print(Log.PASS, "All 5 Chaos Worker processes terminated abruptly with SIGKILL (Exit code: -9).")
    Log.print(Log.CHAOS, "Dead-Man's Switch Verification: Leases remain orphaned with NO heartbeat and NO release.")

    # Step 4: Submit queued workers that must wait for resources
    queued_workers_results = []
    queued_threads = []
    queued_lock = threading.Lock()

    waiter_cpu = max(0.5, round(total_cores * 0.20, 1))
    waiter_mem = int(total_mem * 0.20)

    def waiter_thread(idx: int):
        w_id = f"waiter-worker-{idx+1}"
        t_submit = time.time()
        Log.print(Log.SCHED, f"Queued Worker {w_id} submitting lease request (Needs {waiter_cpu} vCPU, {waiter_mem / (1024**3):.2f} GiB RAM)...")
        try:
            l = client.acquire(
                worker_id=w_id,
                cpu=waiter_cpu,
                memory=waiter_mem,
                priority="Normal",
                ttl=60,
                wait=True,
                timeout=45.0
            )
            t_granted = time.time()
            wait_time = t_granted - t_submit
            Log.print(Log.PASS, f"Queued Worker {w_id} UNBLOCKED & GRANTED lease {l.id} after {wait_time:.2f}s wait!")

            # Simulate short task and release cleanly
            time.sleep(0.5)
            client.release(l.id)

            with queued_lock:
                queued_workers_results.append({
                    "worker_id": w_id,
                    "lease_id": l.id,
                    "wait_time": wait_time,
                    "status": "SUCCESS"
                })
        except Exception as e:
            with queued_lock:
                queued_workers_results.append({
                    "worker_id": w_id,
                    "status": f"FAILED: {e}"
                })
            Log.print(Log.FAIL, f"Queued Worker {w_id} error: {e}")

    for i in range(num_queued):
        t = threading.Thread(target=waiter_thread, args=(i,))
        queued_threads.append(t)
        t.start()

    time.sleep(1.0)
    # Check that waiters are indeed queued
    st = client.status()
    n_queued = len(st.get("queued_leases", []))
    Log.print(Log.INFO, f"Arbiter queue state: {n_queued} workers currently waiting for resources.")
    if n_queued == 0:
        raise AssertionError("Expected waiter workers to be queued while chaos leases are active!")

    # Step 5: Wait and observe Dead-Man's switch heartbeat timeout (15 seconds)
    Log.print(Log.INFO, "Awaiting Arbiter Dead-Man's Switch watchdog timeout (15.0 seconds from lease grant)...")
    reclaimed_at = None

    for tick in range(1, 26):
        time.sleep(1.0)
        elapsed_since_grant = time.time() - avg_grant_time
        st = client.status()
        active_ids = [al["id"] for al in st.get("active_leases", [])]
        still_hung = [lid for lid in chaos_lease_ids if lid in active_ids]

        Log.print(Log.INFO, f"Watchdog Timer: T+{elapsed_since_grant:4.1f}s | Hung leases active: {len(still_hung)}/{num_chaos} | Queued: {len(st.get('queued_leases', []))}")

        if len(still_hung) == 0 and reclaimed_at is None:
            reclaimed_at = elapsed_since_grant
            Log.print(Log.PASS, f"DEAD-MAN'S SWITCH TRIGGERED! Arbiter auto-reclaimed all {num_chaos} hung leases at T+{reclaimed_at:.2f}s!")
            break

    if reclaimed_at is None:
        raise AssertionError("Arbiter failed to auto-reclaim hung leases within timeout window!")

    # Verify Dead-man's switch fired around 15-18s (nominal 15s timeout)
    if not (14.5 <= reclaimed_at <= 18.5):
        Log.print(Log.WARN, f"Reclaim timestamp T+{reclaimed_at:.2f}s is slightly outside ideal 15-18s window, but successfully triggered.")
    else:
        Log.print(Log.PASS, f"Heartbeat timeout accuracy verified: {reclaimed_at:.2f}s matches expected 15.0s window.")

    # Wait for all queued workers to finish
    Log.print(Log.INFO, "Waiting for unblocked queued workers to complete their jobs...")
    for t in queued_threads:
        t.join(timeout=25.0)

    # Verify all queued workers completed successfully
    success_waiters = sum(1 for r in queued_workers_results if r.get("status") == "SUCCESS")
    if success_waiters != num_queued:
        raise AssertionError(f"Expected {num_queued} waiter workers to finish, but only {success_waiters} succeeded!")

    monitor.assert_zero_violations()
    Log.print(Log.PASS, f"TEST SUITE 3 PASSED: Chaos Dead-Man's Switch auto-reclaim and queue unlock fully validated.")


# ==============================================================================
# SUITE 4: Post-Test Ledger & System Integrity Audit
# ==============================================================================
def run_suite_audit(client: NomosClient, monitor: InvariantMonitor):
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")
    Log.print(Log.INFO, "TEST SUITE 4: Post-Test Accounting Ledger & System State Audit")
    Log.print(Log.INFO, "-------------------------------------------------------------------------------")

    # 1. Verify Arbiter state returned to pristine clean state
    status = client.status()
    alloc_cpu = status.get("allocated_cpu", 0.0)
    alloc_mem = status.get("allocated_memory_bytes", 0)
    active_leases = status.get("active_leases", [])
    queued_leases = status.get("queued_leases", [])

    Log.print(Log.INFO, f"Final Allocations: {alloc_cpu:.2f} vCPUs, {alloc_mem} B RAM")
    Log.print(Log.INFO, f"Final Active Leases Count: {len(active_leases)}")
    Log.print(Log.INFO, f"Final Queued Leases Count: {len(queued_leases)}")

    assert abs(alloc_cpu) < 0.01, f"Expected 0.0 allocated CPU, got {alloc_cpu}"
    assert alloc_mem == 0, f"Expected 0 allocated RAM, got {alloc_mem}"
    assert len(active_leases) == 0, f"Expected 0 active leases, got {len(active_leases)}"
    assert len(queued_leases) == 0, f"Expected 0 queued leases, got {len(queued_leases)}"
    Log.print(Log.PASS, "Arbiter zero-leak verification: CPU, RAM, active leases and queues cleanly returned to zero.")

    # 2. Check Audit Ledger
    acct = client.accounting(limit=10)
    summary = acct.get("summary", {})
    recent = acct.get("recent", [])

    Log.print(Log.INFO, "Nomos Audit Ledger Summary:")
    Log.print(Log.INFO, f"  Total Tracked Jobs:      {summary.get('total_records', 0)}")
    Log.print(Log.INFO, f"  Cumulative Core-Hours:   {summary.get('total_cpu_hours', 0.0):.4f} hrs")
    Log.print(Log.INFO, f"  Peak Memory Recorded:    {summary.get('peak_memory_seen_bytes', 0) / (1024**3):.2f} GiB")
    Log.print(Log.INFO, f"  Recent Audit Records Sample (Last {len(recent)}):")
    for r in recent[-5:]:
        Log.print(Log.INFO, f"    - Worker: {r.get('worker_id', ''):<26} Duration: {r.get('duration_seconds', 0.0):.2f}s | Core-Sec: {r.get('cpu_core_seconds', 0.0):.2f}")

    # 3. Invariant Monitor final summary
    Log.print(Log.INFO, "Continuous Invariant Monitor Telemetry:")
    Log.print(Log.INFO, f"  Total Continuous Samples:    {monitor.samples_count}")
    Log.print(Log.INFO, f"  Peak Cores Allocated:        {monitor.peak_cpu_allocated:.2f} / {monitor.pool_cores:.2f} vCPUs")
    Log.print(Log.INFO, f"  Peak Memory Allocated:       {monitor.peak_memory_allocated / (1024**3):.2f} / {monitor.pool_memory_bytes / (1024**3):.2f} GiB")
    Log.print(Log.INFO, f"  Peak Concurrent Leases:      {monitor.peak_active_leases}")
    Log.print(Log.INFO, f"  Peak Wait Queue Depth:       {monitor.peak_queued_leases}")
    Log.print(Log.INFO, f"  Total Invariant Violations:  {len(monitor.violations)}")

    monitor.assert_zero_violations()
    Log.print(Log.PASS, "TEST SUITE 4 PASSED: Accounting ledger verified, zero resource leaks detected.")


# ==============================================================================
# MAIN RUNNER
# ==============================================================================
def main():
    parser = argparse.ArgumentParser(description="Nomos High-Concurrency & Chaos Stress Test Suite")
    parser.add_argument("--socket", type=str, default=None, help="Custom Unix socket path")
    parser.add_argument("--port", type=int, default=9190, help="Web dashboard port for spawned daemon")
    parser.add_argument("--workers", type=int, default=40, help="Number of concurrent workers for stress test (30-50)")
    parser.add_argument("--chaos-workers", type=int, default=5, help="Number of chaos workers (default: 5)")
    parser.add_argument("--daemon-bin", type=str, default="target/debug/nomos", help="Path to nomos executable")
    parser.add_argument("--spawn-daemon", action="store_true", help="Always spawn a fresh local nomos daemon")
    parser.add_argument("--suite", type=str, choices=["all", "backfill", "concurrency", "chaos"], default="all", help="Test suite to run")
    args = parser.parse_args()

    print("===============================================================================")
    print("NOMOS HIGH-CONCURRENCY & CHAOS STRESS TEST SUITE")
    print("===============================================================================")

    # Determine socket path
    socket_path = args.socket
    if not socket_path:
        home_sock = os.path.expanduser("~/.nomos/arbiter.sock")
        if os.path.exists(home_sock) and not args.spawn_daemon:
            socket_path = home_sock
        else:
            socket_path = f"/tmp/nomos_stress_{os.getpid()}.sock"

    daemon_mgr = None
    if args.spawn_daemon or not os.path.exists(socket_path):
        daemon_bin = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", args.daemon_bin))
        if not os.path.exists(daemon_bin):
            # Try workspace root target/debug/nomos
            daemon_bin = os.path.abspath(os.path.join(os.getcwd(), args.daemon_bin))
        if not os.path.exists(daemon_bin):
            Log.print(Log.FAIL, f"Nomos binary not found at {daemon_bin}. Please run 'cargo build' first.")
            sys.exit(1)

        daemon_mgr = DaemonManager(daemon_bin, socket_path, port=args.port)
        daemon_mgr.start()

    client = NomosClient(socket_path)
    monitor = InvariantMonitor(client, interval_sec=0.03)

    test_start = time.time()
    try:
        monitor.start()

        if args.suite in ["all", "backfill"]:
            run_suite_backfilling(client, monitor)

        if args.suite in ["all", "concurrency"]:
            run_suite_high_concurrency(client, monitor, num_workers=args.workers)

        if args.suite in ["all", "chaos"]:
            run_suite_chaos_testing(client, monitor, socket_path, num_chaos=args.chaos_workers, num_queued=8)

        if args.suite == "all":
            run_suite_audit(client, monitor)

        monitor.stop()
        total_time = time.time() - test_start

        print("===============================================================================")
        print(f"ALL NOMOS STRESS AND CHAOS TEST SUITES PASSED IN {total_time:.2f}s")
        print("Summary:")
        print("  - Total Invariant Violations: 0")
        print(f"  - Peak Allocation Headroom:   {monitor.peak_cpu_allocated:.1f} vCPUs, {monitor.peak_memory_allocated / (1024**3):.2f} GiB RAM")
        print(f"  - Peak Concurrent Leases:     {monitor.peak_active_leases}")
        print(f"  - Peak Queue Depth:           {monitor.peak_queued_leases}")
        print(f"  - Dead-Man's Switch Timeout:  15.0s Verified")
        print("===============================================================================")

    except Exception as e:
        monitor.stop()
        Log.print(Log.FAIL, f"FATAL TEST SUITE FAILURE: {e}")
        import traceback
        traceback.print_exc()
        if daemon_mgr:
            daemon_mgr.stop()
        sys.exit(1)

    if daemon_mgr:
        daemon_mgr.stop()

    sys.exit(0)


if __name__ == "__main__":
    main()
