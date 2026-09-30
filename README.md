# NOMOS (Νόμος) — Single-Host Delicate Resource Arbiter

A deterministic, zero-GC, ultra-low-overhead resource arbiter written in 100% Rust. Nomos manages delicate host compute (CPU, RAM, iGPU, NPU), temporary scratch storage, inter-worker DAG dependencies, and real-time scheduling with 3D Spatio-Temporal Bin Packing.

---

## Features

- **Host Safety Guarantee**: Strict reservation protects host OS memory and unmanaged CPU cores (e.g. 50% CPU, 50% RAM, 8GB host reserve floor).
- **Spatio-Temporal Scheduling**: Integrates task runtime estimation, deadlines, and Backfilling to prevent short tasks (2s) from starving behind long batch jobs (3h).
- **Hardware Device Leases**: Arbitrates access to Intel Arc iGPU (`/dev/dri/renderD128`) and Intel Meteor Lake NPU (`/dev/accel/accel0`).
- **DAG Workflow Engine**: Supports dependency graphs (`depends_on`) with automatic fan-in worker unlocking and queue progression.
- **Dead-Man's Switch**: Automatically reclaims resources from hung or deadlocked workers via 15-second heartbeat watchdogs.
- **Audit Accounting Ledger**: Records vCPU-core-seconds, peak memory, and GPU usage into persistent `audit.jsonl`.
- **Zero-Code Wrapper (`nomos run`)**: Wrap any CLI, Python, or shell script under a strict Nomos lease without modifying code.
- **Python Client SDK**: Pure standard-library Python SDK with context managers (`with nomos.lease(...):`).
- **Embedded Visualizer**: Web dashboard on port 9100 (Axum + Canvas Gantt + SSE) and Terminal TUI (`nomos top`).

---

## One-Command Installation & Update

Install or update Nomos, its background service, system PATH, configuration, and Python SDK on any host (macOS or Linux) with a single command:

```bash
# Install or Update via installer script
./install.sh

# Or once installed, self-update at any time directly via CLI:
nomos update
```

---

## Quickstart

### 1. Build from Source
```bash
cargo build --release
```
The compiled static binary is located at `target/release/nomos`.

### 2. Probe Host Hardware
```bash
./target/release/nomos probe
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
./target/release/nomos daemon --config nomos.toml --port 9100
```

### 4. Check Status
```bash
./target/release/nomos status
```

### 5. Wrap and Execute Any Command
```bash
./target/release/nomos run --cpu 2 --mem 4GB -- python3 -c 'print("Running inside Nomos lease!")'
```

### 6. Interactive Terminal Monitor (TUI)
```bash
./target/release/nomos top
```

### 7. View Live Web Dashboard
Open your browser at `http://localhost:9100`.

---

## Python SDK

Nomos includes a zero-dependency Python client located at `sdk/python/nomos.py`.

```python
import nomos
import time

# 1. Automatic context manager with background heartbeat thread
with nomos.lease(cpu=2.0, memory="4GB", devices=["arc"], ttl=60) as l:
    print(f"Acquired Lease {l.id} on slice {l.cgroup_path}")
    # Perform heavy AI / computation workload
    time.sleep(5)

# 2. Directed Acyclic Graph (DAG) Pipeline
with nomos.lease(worker_id="etl", cpu=2.0, memory="4GB"):
    # Step 1: Preprocessing
    pass

with nomos.lease(worker_id="train", cpu=4.0, memory="8GB", depends_on=["etl"]):
    # Step 2: Executes only after "etl" completes cleanly
    pass
```

---

## Multi-Worker DAG Demonstration

Run the automated 3-worker pipeline demo (Worker 1: ETL, Worker 2: Feature Engineering, Worker 3: Model Training depending on 1 & 2):

```bash
# Python SDK Version
python3 examples/dag_pipeline_demo.py

# CLI Runner Version
./examples/dag_pipeline_demo.sh
```

---

## Model Context Protocol (MCP) Integration for LLMs

Nomos natively implements the Model Context Protocol (MCP) over `stdio` via JSON-RPC 2.0. This allows AI assistants like Claude Desktop, Cursor, Claude Code, Cline, and Windsurf to execute shell commands, train models, run tests, and probe hardware strictly bounded by Nomos resource budgets and network isolation.

### Claude Desktop & Cursor Configuration

Add Nomos to your `claude_desktop_config.json` (on macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`, on Linux: `~/.config/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "nomos": {
      "command": "nomos",
      "args": ["mcp"]
    }
  }
}
```

Or specify the absolute binary path (e.g. `/usr/local/bin/nomos` or `/home/<user>/.local/bin/nomos`).

### Tools Exposed to LLMs

| Tool | Parameters | Description |
|---|---|---|
| `nomos_run` | `command`, `cpu`, `memory`, `devices`, `network`, `priority`, `timeout_seconds`, `working_dir` | Execute shell command bounded by vCPU, RAM, GPU/NPU, and network isolation mode (`isolated`, `none`, `host`). Prevents OOM crashes. |
| `nomos_probe` | None | Probe physical host hardware telemetry (Cores, RAM, Swap, Storage, GPU, NPU). |
| `nomos_status` | None | Query current arbiter pool headroom, active leases, and queued waiting requests. |
| `nomos_accounting` | `limit` | Query historical resource audit ledger and cumulative CPU/GPU core-hours. |
| `nomos_acquire` | `worker_id`, `cpu`, `memory`, `priority`, `ttl_seconds` | Manually acquire an explicit resource lease for multi-step workflows. |
| `nomos_release` | `lease_id` | Release an explicitly acquired resource lease. |

---

## Systemd Service Installation (Linux Host)

Install Nomos as a background systemd service with Cgroups v2 delegation:

```bash
sudo cp packaging/systemd/nomos.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now nomos.service
sudo systemctl status nomos.service
```

---

## Workspace Structure

```
nomos/
├── crates/
│   ├── nomos-core/       # Pure logic: Budgeting, Leases, DAG, Scheduler, Accounting
│   ├── nomos-sys/        # Hardware probe, Cgroups v2, SO_PEERCRED, Thermal sensors
│   └── nomos-daemon/     # Tokio server, Unix IPC, Axum Web GUI, TUI, Runner CLI
├── sdk/
│   └── python/           # Zero-dependency Python SDK (nomos.py)
├── examples/             # 3-Worker DAG pipeline demos (Python & Bash)
├── packaging/
│   └── systemd/          # Systemd unit file with Delegate=yes
├── nomos.toml            # Reference configuration file
└── Cargo.toml            # Multi-crate Cargo workspace
```
