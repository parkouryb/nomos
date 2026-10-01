# NOMOS (Νόμος) — Single-Host Delicate Resource Arbiter

A deterministic, zero-GC, ultra-low-overhead resource arbiter written in 100% Rust. Nomos manages delicate host compute (CPU, RAM, iGPU, NPU), temporary scratch storage, inter-worker DAG dependencies, and real-time scheduling with 3D Spatio-Temporal Bin Packing.

---

## Features

- **Host Safety Guarantee**: Strict reservation protects host OS memory and unmanaged CPU cores (e.g. 50% CPU, 50% RAM, 8GB host reserve floor).
- **AI Assistant Auto-Integration**: One-command installer automatically configures **Google Antigravity (AGY)** and **Claude Desktop** with native MCP servers and deterministic lifecycle hooks.
- **Universal Tool Governance**: Auto-intercepts LLM bash execution, subagent swarms (`invoke_subagent`), and GPU vision tasks (`generate_image`), estimating runtime durations and enforcing TTLs.
- **Spatio-Temporal Scheduling**: Integrates task runtime estimation, deadlines, and Backfilling to prevent short tasks (2s) from starving behind long batch jobs (3h).
- **Hardware Device Leases**: Arbitrates access to Apple Silicon Metal GPU / ANE and Linux Intel Arc iGPU / NPU.
- **DAG Workflow Engine**: Supports dependency graphs (`depends_on`) with automatic fan-in worker unlocking and queue progression.
- **Dead-Man's Switch**: Automatically reclaims resources from hung or deadlocked workers via 15-second heartbeat watchdogs.
- **Audit Accounting Ledger**: Records vCPU-core-seconds, peak memory, and GPU usage into persistent `audit.jsonl` with 30-day retention and auto-pruning.
- **Zero-Code Wrapper (`nomos run`)**: Wrap any CLI, Python, or shell script under a strict Nomos lease without modifying code.
- **Python Client SDK**: Pure standard-library Python SDK with context managers (`with nomos.lease(...):`).
- **Embedded Web GUI & TUI**: Interactive multi-tab Web dashboard on port 9100 (Axum + Live Canvas Chart + Job History & Audit + Hardware Telemetry) and Terminal TUI (`nomos top`).

---

## One-Command Installation & Update

Install or update Nomos, background service daemon, PATH, configuration, Python SDK, and automatically configure **Antigravity (AGY)** and **Claude Desktop** with a single command:

```bash
# Clone and install
git clone https://github.com/parkouryb/nomos.git
cd nomos
./install.sh

# Or once installed, self-update at any time directly via CLI:
nomos update
```

The installer automatically detects and registers:
- **Antigravity CLI**: Installs lifecycle hook (`~/.gemini/config/scripts/nomos_hook.py`), configures `hooks.json`, and registers Nomos MCP server in `mcp_config.json`.
- **Claude Desktop**: Automatically registers the `nomos` MCP server in `claude_desktop_config.json`.

---

## Interactive Web Dashboard (`http://localhost:9100`)

Nomos embeds a zero-dependency, real-time Web GUI on port **9100** featuring:

1. **Live Monitor Tab**:
   - Real-time CPU & RAM pool allocation gauges.
   - Smooth 60-second rolling Canvas activity chart updated via Server-Sent Events (SSE).
   - Live table of Active & Queued worker leases.
2. **Job History & Audit Tab**:
   - Summary metrics (Total tracked jobs, Cumulative CPU-hours, Peak memory, Cumulative runtime).
   - Search & filter completed jobs by Worker ID, Lease ID, or State (`Completed`, `Terminated`, `Timeout`).
3. **Host Hardware & Pool Tab**:
   - Host physical core count, RAM, swap, and disk storage.
   - Hardware accelerator detection (Metal GPU, Intel Arc, Neural Engine).
   - Pool limits and host reserve floor.

---

## Quickstart

### 1. Probe Host Hardware
```bash
nomos probe
```

### 2. Check Arbiter Status
```bash
nomos status
```

### 3. Wrap and Execute Any Command
```bash
nomos run --cpu 2.0 --mem 4GB --ttl 60 -- python3 -c 'print("Running inside Nomos lease!")'
```

### 4. Interactive Terminal Monitor (TUI)
```bash
nomos top
```

### 5. Inspect Audit Accounting History
```bash
nomos accounting --limit 50 --days 30
```

---

## Antigravity (AGY) Integration

When running under Google Antigravity, Nomos provides a universal `PreToolUse` lifecycle hook (`nomos_hook.py`) that governs all agent actions:

- **`run_command`**: Classifies commands into resource tiers (`LIGHT_METADATA`, `MEDIUM_SCRIPT_TEST`, `HEAVY_COMPILATION_BENCHMARK`), estimates safe execution durations and TTL buffers, and transparently wraps shell commands into `nomos run`.
- **`invoke_subagent`**: Queries the Nomos arbiter daemon via low-latency Unix socket IPC to ensure sufficient CPU/RAM headroom before launching subagent swarms.
- **`generate_image`**: Protects the GPU / Unified Memory envelope against desktop freezes.

---

## Claude Desktop & Cursor MCP Integration

Nomos natively implements the Model Context Protocol (MCP) over `stdio` via JSON-RPC 2.0.

Configuration (auto-configured by `install.sh` or manually added to `claude_desktop_config.json`):

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

### Tools Exposed to LLMs

| Tool | Parameters | Description |
|---|---|---|
| `nomos_run` | `command`, `cpu`, `memory`, `devices`, `network`, `priority`, `timeout_seconds`, `working_dir` | Execute shell command bounded by vCPU, RAM, GPU/NPU, and network isolation mode (`isolated`, `none`, `host`). Prevents OOM crashes. |
| `nomos_probe` | None | Probe physical host hardware telemetry (Cores, RAM, Swap, Storage, GPU, NPU). |
| `nomos_status` | None | Query current arbiter pool headroom, active leases, and queued waiting requests. |
| `nomos_accounting` | `limit`, `days` | Query historical resource audit ledger and cumulative CPU/GPU core-hours. |
| `nomos_acquire` | `worker_id`, `cpu`, `memory`, `priority`, `ttl_seconds` | Manually acquire an explicit resource lease for multi-step workflows. |
| `nomos_release` | `lease_id` | Release an explicitly acquired resource lease. |

---

## Python SDK

Nomos includes a zero-dependency Python client located at `sdk/python/nomos.py`.

```python
import nomos
import time

# 1. Automatic context manager with background heartbeat thread
with nomos.lease(cpu=2.0, memory="4GB", devices=["gpu"], ttl=60) as l:
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

## Workspace Structure

```
nomos/
├── crates/
│   ├── nomos-core/       # Budgeting, Leases, DAG, Scheduler, Accounting
│   ├── nomos-sys/        # Hardware probe, Cgroups v2, SO_PEERCRED, Thermal sensors
│   └── nomos-daemon/     # Tokio server, Unix IPC, Axum Web GUI, TUI, Runner CLI, MCP
├── sdk/
│   └── python/           # Zero-dependency Python SDK (nomos.py)
├── examples/             # 3-Worker DAG pipeline demos (Python & Bash)
├── packaging/
│   ├── integrations/     # Ready-to-use Antigravity & Claude Desktop configs/hooks
│   ├── launchd/          # macOS LaunchAgent plist
│   └── systemd/          # Linux Systemd unit file with Delegate=yes
├── install.sh            # Universal 1-command installer & updater
├── nomos.toml            # Reference configuration file
└── Cargo.toml            # Multi-crate Cargo workspace
```

---

## License & Commercial Use

Nomos is source-available and licensed under the **[PolyForm Noncommercial License 1.0.0](LICENSE)**.

- **Noncommercial Use:** Free for personal use, learning, academic research, and noncommercial evaluation.
- **Commercial Use:** Commercial purposes (such as running in commercial production, enterprise agent fleets, or embedding into commercial offerings) require a separate commercial license.

For commercial licensing agreements, custom enterprise integrations, or support inquiries, please contact:
- **Author:** Hieu ([@parkouryb](https://github.com/parkouryb))
- **Email:** [147523689a@gmail.com](mailto:147523689a@gmail.com)

