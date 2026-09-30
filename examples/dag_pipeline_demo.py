#!/usr/bin/env python3
"""
Nomos 3-Worker DAG Pipeline Demonstration
Demonstrates:
  1. Worker 1 (etl-preprocess): 2 cores, 4GB RAM
  2. Worker 2 (feature-engineering): 2 cores, 4GB RAM
  3. Worker 3 (model-training): 3 cores, 8GB RAM, depends on [Worker 1, Worker 2]
All 3 workers are submitted simultaneously. Nomos dynamically arbitrates
resources and resolves DAG dependencies with zero collisions.
"""

import sys
import os
import time
import threading

# Add sdk/python to sys.path
sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "sdk", "python")))
import nomos

def run_worker_1():
    print("[Worker 1: ETL] Submitting lease request (2.0 Cores, 4GB RAM)...")
    with nomos.lease(
        worker_id="etl-preprocess",
        cpu=2.0,
        memory="4GB",
        ttl=30,
    ) as l:
        print(f"[Worker 1: ETL] GRANTED Lease {l.id}! Executing data ingestion & normalization...")
        time.sleep(2.5)
        print(f"[Worker 1: ETL] Finished task cleanly. Releasing Lease {l.id}...")

def run_worker_2():
    print("[Worker 2: Feature] Submitting lease request (2.0 Cores, 4GB RAM)...")
    with nomos.lease(
        worker_id="feature-engineering",
        cpu=2.0,
        memory="4GB",
        ttl=30,
    ) as l:
        print(f"[Worker 2: Feature] GRANTED Lease {l.id}! Extracting high-dimensional features...")
        time.sleep(3.5)
        print(f"[Worker 2: Feature] Finished task cleanly. Releasing Lease {l.id}...")

def run_worker_3():
    print("[Worker 3: Train] Submitting lease request (3.0 Cores, 8GB RAM) with dependencies [etl-preprocess, feature-engineering]...")
    with nomos.lease(
        worker_id="model-training",
        cpu=3.0,
        memory="8GB",
        depends_on=["etl-preprocess", "feature-engineering"],
        ttl=30,
    ) as l:
        print(f"[Worker 3: Train] GRANTED Lease {l.id}! All DAG dependencies fulfilled! Running model training...")
        time.sleep(2.0)
        print(f"[Worker 3: Train] Model training converged! Releasing Lease {l.id}...")

def main():
    print("===============================================================================")
    print("NOMOS 3-WORKER DAG PIPELINE DEMONSTRATION")
    print("===============================================================================")
    client = nomos.NomosClient()

    # Query initial status
    status = client.status()
    pool = status["pool"]
    print(f"Nomos Budget Pool: {pool['total_cores']} Cores | {pool['total_memory_bytes'] / (1024**3):.1f} GB RAM")
    print(f"Active leases: {len(status['active_leases'])}, Queued leases: {len(status['queued_leases'])}")
    print("-------------------------------------------------------------------------------")
    print("Launching Workers 1, 2, and 3 concurrently in background threads...")

    t1 = threading.Thread(target=run_worker_1)
    t2 = threading.Thread(target=run_worker_2)
    t3 = threading.Thread(target=run_worker_3)

    start = time.time()
    t1.start()
    t2.start()
    time.sleep(0.2)
    t3.start()

    t1.join()
    t2.join()
    t3.join()

    elapsed = time.time() - start
    print("-------------------------------------------------------------------------------")
    print(f"Pipeline finished successfully in {elapsed:.2f} seconds!")
    print("===============================================================================")

    # Print accounting report
    acct = client.accounting(limit=5)
    summary = acct["summary"]
    print("NOMOS AUDIT LEDGER - RECENT JOBS:")
    print(f"Total jobs recorded: {summary['total_records']}")
    for r in acct["recent"][-3:]:
        print(f" - Lease: {r['lease_id']} | Worker: {r['worker_id']} | Duration: {r['duration_seconds']:.2f}s | CPU-sec: {r['cpu_core_seconds']:.2f}")
    print("===============================================================================")

if __name__ == "__main__":
    main()
