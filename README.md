# Nomos 0.2 — kernel-enforced resource leases

Nomos executes commands under CPU and RAM limits on Linux cgroup v2. A delegated systemd user service with CPU and memory controllers is required. Missing delegation, unsupported resources or failed kernel writes are errors; there is no simulated enforcement fallback.

## Install and run

Build from this checkout with `./install.sh` on Linux/systemd. The installer creates a user service with `Delegate=yes`, then checks that the arbiter answers. Updating the daemon interrupts its existing workloads. macOS supports builds, probes and offline tests, but cannot execute enforced leases.

```sh
nomos status
nomos run --network host --cpu 1 --mem 256MB --ttl 60 -- python3 -c 'print("hello")'
```

The runner attaches the child to its lease cgroup before executing the command. CPU, RAM and swap limits are read back from the kernel. Heartbeat loss, timeout and cancellation kill descendants before returning capacity. A failed job does not unlock DAG dependencies. SSH callers are moved into a systemd user scope when needed.

Host networking must be selected explicitly. Network isolation, devices/GPU isolation, scratch quotas, bandwidth and IOPS are unsupported and rejected. Hardware discovery does not mean that a resource can be enforced.

## Python and MCP

The installer places the SDK in `~/.nomos/sdk-venv`; use that environment's Python interpreter. It does not modify managed system Python.

```python
import nomos
result = nomos.run(
    ["python3", "-c", "print('hello')"],
    network="host", cpu=1, memory="256MB", ttl=60,
    check=True, capture_output=True, text=True,
)
print(result.stdout)
```

`nomos.run` uses the same enforced CLI runner. The former `with nomos.lease(...)` context manager raises an error: it never attached its caller to the lease. Raw acquire/release is a reservation API, not proof of process containment. Existing clients must migrate.

`nomos mcp` serves stdio MCP. Its execution tool also invokes the enforced runner, requires explicit host networking and propagates execution failures. Reservation tools do not themselves execute or isolate a workload. Historical assistant hooks and examples are not an enforcement boundary; invoke the runner for actual work.

## Observation

`nomos accounting` exposes recorded usage. CPU and memory measurement provenance is stored separately from unavailable GPU/network telemetry; historical unmeasured values are excluded from measured totals. Reserved capacity is not measured usage. The dashboard binds to loopback on port 9100. The local IPC socket is mode 0600.

ComposeLab uses Nomos leases for each deployed service and validates live processes, cgroup limits and application smoke checks before committing a release. See the ComposeLab deployment guide for the stack-level contract.

## License & Commercial Use

Nomos is source-available and licensed under the **[PolyForm Noncommercial License 1.0.0](LICENSE)**.

- **Noncommercial Use:** Free for personal use, learning, academic research, and noncommercial evaluation.
- **Commercial Use:** Commercial purposes (such as running in commercial production, enterprise agent fleets, or embedding into commercial offerings) require a separate commercial license.

For commercial licensing agreements, custom enterprise integrations, or support inquiries, please contact:
- **Author:** Hieu ([@parkouryb](https://github.com/parkouryb))
- **Email:** [147523689a@gmail.com](mailto:147523689a@gmail.com)

