# NOMOS (Νόμος) — Single-Host Delicate Resource Arbiter

A deterministic, zero-GC, ultra-low-overhead resource arbiter written in 100% Rust. Nomos manages delicate host compute (CPU, RAM, iGPU, NPU), temporary scratch storage, inter-worker DAG dependencies, and real-time scheduling with 3D Spatio-Temporal Bin Packing.

---

## Features

- **Host Safety Guarantee**: Strict reservation protects host OS memory and unmanaged CPU cores (e.g. 50% CPU, 50% RAM, 8GB host reserve floor).
- **Spatio-Temporal Scheduling**: Integrates task runtime estimation, deadlines, and Backfilling to prevent short tasks (2s) from starving behind long batch jobs (3h).
- **Hardware Device Leases**: Arbitrates access to Intel Arc iGPU (`/dev/dri/renderD128`) and Intel Meteor Lake NPU (`/dev/accel/accel0`).
- **Dead-Man's Switch**: Automatically reclaims resources from hung or deadlocked workers via 10-second heartbeats.
- **Audit Accounting Ledger**: Records vCPU-core-seconds, peak memory, and GPU usage into `audit.jsonl`.
- **Zero-Code Wrapper (`nomos run`)**: Wrap any CLI, Python, or shell script under a strict Nomos lease without modifying code.
- **Embedded Visualizer**: Web dashboard on port 9100 (Axum + Canvas Gantt + SSE) and Terminal TUI (`nomos top`).

---

## Quickstart

### 1. Build
```bash
cargo build --release
```

The compiled static binary will be located at `target/release/nomos`.

### 2. Probe Host Hardware
```bash
./nomos probe
```

Output:
```
========================================================
 NOMOS HARDWARE PROBE - Host Telemetry
========================================================
 Physical CPU Cores:      14
 Physical RAM:            60.12 GiB
 Physical Swap:           8.00 GiB
 Total Storage Space:     915.22 GiB
--------------------------------------------------------
 GPU Accelerator:         DETECTED
  - Linux DRM Render Node: /dev/dri/renderD128
 NPU Accelerator:         DETECTED
  - Linux NPU Accelerator: /dev/accel/accel0
========================================================
```

### 3. Start Background Daemon
```bash
./nomos daemon --config nomos.toml --port 9100
```

### 4. Check Status
```bash
./nomos status
```

### 5. Wrap and Execute Any Command
```bash
./nomos run --cpu 2 --mem 4GB -- python3 -c 'print("Running inside Nomos lease!")'
```

### 6. Interactive Terminal Monitor (TUI)
```bash
./nomos top
```

### 7. View Live Web Dashboard
Open your browser at `http://localhost:9100`.

---

## Architecture

```
nomos/
├── crates/
│   ├── nomos-core/   # Pure logic: Budgeting, Leases, DAG, Scheduler, Accounting
│   ├── nomos-sys/    # Hardware: /proc & sysinfo probe, Cgroups v2, SO_PEERCRED, Thermal
│   └── nomos-daemon/ # Tokio server, Unix socket IPC, Axum Web GUI, TUI, Runner CLI
```
