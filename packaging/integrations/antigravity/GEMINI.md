# Antigravity Rules - Host Execution & Delicate Resource Safety

## MANDATORY: Nomos Resource Arbiter Enforcement
This host is strictly regulated by Nomos Resource Arbiter to prevent CPU starvation, memory exhaustion (OOM), and desktop UI freezes.

### Universal Tool Governance via Nomos Lifecycle Hooks:
The automated Antigravity hook (`nomos_hook.py`) intercepts and coordinates AGY tools across the system:

1. **Terminal / Bash Tool (`run_command`)**:
   - **Workload & Runtime Duration Estimation**:
     - **Quick Queries** (`ls`, `cat`, `git status`, `pwd`):
       - Est. Duration: `< 2s` | Nomos TTL: `--ttl 20` | CPU: `--cpu 0.5` | RAM: `--mem 512MB`
     - **Medium Workloads** (`pytest`, `python3`, `npm test`, `git pull/push`, package installs):
       - Est. Duration: `~15-60s` | Nomos TTL: `--ttl 180` | CPU: `--cpu 2.0` | RAM: `--mem 4GB`
     - **Heavy Builds & Benchmarks** (`cargo build`, `make`, `docker`, benchmark suites):
       - Est. Duration: `~2-5m` | Nomos TTL: `--ttl 600` | CPU: `--cpu 4.0` | RAM: `--mem 8GB`
   - **Automatic Interception**: If a command is invoked without `nomos run`, the hook automatically classifies the tier, estimates safe duration/TTL, and wraps it into Nomos before shell execution.

2. **Subagents Orchestration Tool (`invoke_subagent`)**:
   - Every subagent dispatch is checked against the Nomos pool capacity via low-latency Unix socket IPC.
   - Any nested terminal commands executed by subagents automatically inherit Nomos lease queueing, ensuring subagent swarms never exceed the host CPU/RAM budget.

3. **Media & Image Generation Tool (`generate_image`)**:
   - Intercepted to ensure inference runs within the Nomos GPU / Unified Memory envelope, preventing UI freezes on Apple Silicon.

4. **Multi-Step Workflows (Nomos MCP Tools)**:
   - For complex multi-command procedures, prefer acquiring a long-running lease with `nomos_acquire`, executing work steps, and finalizing with `nomos_release`.
   - Query host capacity with `nomos_probe` and live queue with `nomos_status`.
   - Monitor real-time status and audit history at `http://localhost:9100`.
