# Nomos Python SDK 0.2

Use `nomos.run(argv, network="host", cpu=1, memory="256MB", ttl=60, check=True)` to execute under verified Linux cgroup limits. The `nomos` executable and a healthy delegated arbiter must be installed. Arguments such as `capture_output=True` and `text=True` are passed to `subprocess.run`.

The old `lease` context manager is disabled because it did not attach the caller process to its cgroup. Raw acquire/release only reserves capacity. GPU, isolated networking and scratch quotas are not supported by this version.
