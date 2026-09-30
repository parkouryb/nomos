# Antigravity Rules - Host Execution & Delicate Resource Safety

## MANDATORY: Nomos Resource Arbiter Enforcement
This machine is managed by Nomos Resource Arbiter to prevent CPU starvation, memory exhaustion (OOM), and desktop UI freezes.

### Execution Policy:
1. **Prefer Nomos MCP Tool**: When executing commands, training models, running test suites, or performing heavy operations, ALWAYS invoke the `nomos_run` MCP tool instead of unconstrained shell executions.
2. **Terminal / Bash Fallback**: When using `run_command`:
   - ALWAYS wrap heavy, long-running, or resource-intensive tasks inside Nomos:
     `nomos run --cpu <cores> --mem <ram> --network <isolated|none|host> -- <command>`
   - Examples:
     * Compilation: `nomos run --cpu 4.0 --mem 8GB -- cargo build`
     * Test Suites: `nomos run --cpu 2.0 --mem 4GB --network isolated -- pytest`
     * Python scripts: `nomos run --cpu 2.0 --mem 4GB -- python3 script.py`
     * Benchmarks: `nomos run --cpu 4.0 --mem 8GB -- python3 benches/nomos_bench.py`
   - Lightweight metadata queries (e.g. `ls`, `pwd`, `git status`, `git diff`, `git log`, `cat`) may be run directly without Nomos wrapper.
3. **Hardware Telemetry**: Use `nomos_probe` (or `nomos probe`) to query host capabilities before requesting high resource limits.
4. **Pool Headroom**: Check `nomos_status` (or `nomos status`) before queueing large batch workloads.
