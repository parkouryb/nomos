"""
Nomos Python SDK - Single-Host Delicate Resource Arbiter Client
Pure standard-library Python SDK for communicating with the Nomos arbiter daemon.
"""

import os
import sys
import json
import time
import socket
import threading
from contextlib import contextmanager
from typing import Optional, List, Dict, Any, Union

def default_socket_path() -> str:
    env_sock = os.environ.get("NOMOS_SOCKET")
    if env_sock:
        return env_sock
    run_sock = "/run/nomos/arbiter.sock"
    if os.path.exists("/run") and os.access("/run", os.W_OK):
        return run_sock
    home = os.environ.get("HOME", ".")
    return os.path.join(home, ".nomos", "arbiter.sock")


def parse_bytes(val: Union[int, str]) -> int:
    """Parse string byte descriptions (e.g. '512MB', '4GB', '1TB') to integer bytes."""
    if isinstance(val, int):
        return val
    s = val.strip().upper()
    units = {
        "B": 1,
        "KB": 1024,
        "KIB": 1024,
        "MB": 1024 ** 2,
        "MIB": 1024 ** 2,
        "GB": 1024 ** 3,
        "GIB": 1024 ** 3,
        "TB": 1024 ** 4,
        "TIB": 1024 ** 4,
    }
    for unit, multiplier in sorted(units.items(), key=lambda x: -len(x[0])):
        if s.endswith(unit):
            num_str = s[:-len(unit)].strip()
            return int(float(num_str) * multiplier)
    return int(s)


class NomosError(Exception):
    pass


class Lease:
    def __init__(self, data: Dict[str, Any]):
        self.id = data.get("id", "")
        self.state = data.get("state", "Unknown")
        self.worker_id = data.get("request", {}).get("worker_id", "")
        self.req_cpu = data.get("request", {}).get("req_cpu", 1.0)
        self.req_memory_bytes = data.get("request", {}).get("req_memory_bytes", 0)
        self.scratch_path = data.get("scratch_path")
        self.cgroup_path = data.get("cgroup_path")
        self.raw = data

    def __repr__(self):
        return f"<NomosLease id='{self.id}' worker='{self.worker_id}' cpu={self.req_cpu} mem={self.req_memory_bytes} state='{self.state}'>"


class NomosClient:
    def __init__(self, socket_path: Optional[str] = None):
        self.socket_path = socket_path or default_socket_path()

    def _call(self, request_payload: Any) -> Any:
        if not os.path.exists(self.socket_path):
            raise NomosError(
                f"Nomos socket not found at '{self.socket_path}'. "
                "Ensure 'nomos daemon' is running."
            )

        max_retries = 3
        for attempt in range(max_retries):
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                sock.connect(self.socket_path)
                req_bytes = (json.dumps(request_payload) + "\n").encode("utf-8")
                sock.sendall(req_bytes)

                buffer = ""
                while True:
                    chunk = sock.recv(4096).decode("utf-8")
                    if not chunk:
                        break
                    buffer += chunk
                    if "\n" in buffer:
                        line, _ = buffer.split("\n", 1)
                        return json.loads(line)
                if buffer.strip():
                    return json.loads(buffer.strip())
                raise NomosError("Empty response received from Nomos arbiter")
            except (ConnectionRefusedError, ConnectionResetError, BrokenPipeError, socket.error) as e:
                if attempt == max_retries - 1:
                    raise NomosError(f"Socket connection error after {max_retries} attempts: {e}")
                time.sleep(0.05 * (attempt + 1))
            finally:
                sock.close()

    def query_lease(self, lease_id: str) -> Any:
        return self._call({"QueryLease": {"lease_id": lease_id}})

    def acquire(
        self,
        worker_id: Optional[str] = None,
        cpu: float = 1.0,
        memory: Union[int, str] = "1GB",
        scratch: Optional[Union[int, str]] = None,
        devices: Optional[List[str]] = None,
        network_mode: str = "isolated",
        depends_on: Optional[List[str]] = None,
        priority: str = "Normal",
        ttl: int = 60,
        wait: bool = True,
        timeout: float = 300.0,
    ) -> Lease:
        """
        Request a resource lease from the Nomos arbiter.
        If wait is True, this blocks until resources and DAG dependencies are satisfied.
        """
        mem_bytes = parse_bytes(memory)
        scratch_bytes = parse_bytes(scratch) if scratch else 0
        w_id = worker_id or f"py-{os.getpid()}-{int(time.time())}"

        formatted_devices = []
        if devices:
            for d in devices:
                if "npu" in d.lower():
                    formatted_devices.append({"Npu": d})
                else:
                    formatted_devices.append({"Gpu": d})

        req = {
            "AcquireLease": {
                "worker_id": w_id,
                "tenant": "default",
                "req_cpu": float(cpu),
                "req_memory_bytes": mem_bytes,
                "req_scratch_bytes": scratch_bytes,
                "devices": formatted_devices,
                "network_mode": network_mode.lower(),
                "network_bandwidth_mbps": None,
                "estimated_seconds": None,
                "deadline": None,
                "depends_on": depends_on or [],
                "priority": priority,
                "ttl_seconds": ttl,
            }
        }

        resp = self._call(req)

        if "LeaseGranted" in resp:
            return Lease(resp["LeaseGranted"])

        if "LeaseQueued" in resp:
            queued_info = resp["LeaseQueued"]
            lease_data = queued_info.get("lease", {})
            lease_id = lease_data.get("id")
            pos = queued_info.get("position", 1)
            reason = queued_info.get("reason", "")

            if not wait:
                raise NomosError(f"Lease queued at position #{pos}: {reason}")

            start_time = time.time()
            while time.time() - start_time < timeout:
                time.sleep(0.3)
                poll_resp = self.query_lease(lease_id)
                if "LeaseGranted" in poll_resp:
                    return Lease(poll_resp["LeaseGranted"])
                if "Error" in poll_resp:
                    raise NomosError(f"Lease failed while queued: {poll_resp['Error']}")

            raise NomosError(f"Timed out after {timeout}s waiting for lease {lease_id}")

        if "Error" in resp:
            raise NomosError(f"Nomos rejected lease: {resp['Error']}")

        raise NomosError(f"Unexpected arbiter response: {resp}")

    def heartbeat(self, lease_id: str) -> bool:
        resp = self._call({"Heartbeat": {"lease_id": lease_id}})
        if "HeartbeatAck" in resp:
            return resp["HeartbeatAck"].get("success", False)
        return False

    def release(self, lease_id: str) -> bool:
        resp = self._call({"ReleaseLease": {"lease_id": lease_id}})
        return "ReleasedAck" in resp

    def status(self) -> Dict[str, Any]:
        resp = self._call("GetStatus")
        if "Status" in resp:
            return resp["Status"]
        raise NomosError(f"Failed to get status: {resp}")

    def accounting(self, limit: int = 20) -> Dict[str, Any]:
        resp = self._call({"GetAccounting": {"limit": limit}})
        if "Accounting" in resp:
            return resp["Accounting"]
        raise NomosError(f"Failed to get accounting: {resp}")


@contextmanager
def lease(
    cpu: float = 1.0,
    memory: Union[int, str] = "1GB",
    scratch: Optional[Union[int, str]] = None,
    devices: Optional[List[str]] = None,
    network_mode: str = "isolated",
    worker_id: Optional[str] = None,
    depends_on: Optional[List[str]] = None,
    priority: str = "Normal",
    ttl: int = 60,
    socket_path: Optional[str] = None,
    timeout: float = 300.0,
):
    """
    Context manager that acquires a Nomos lease, runs a background dead-man's switch
    heartbeat thread, and automatically releases the lease on block exit.

    Example:
        with nomos.lease(cpu=2.0, memory="4GB", network_mode="none"):
            # Air-gapped confidential task
            pass
    """
    client = NomosClient(socket_path=socket_path)
    l = client.acquire(
        worker_id=worker_id,
        cpu=cpu,
        memory=memory,
        scratch=scratch,
        devices=devices,
        network_mode=network_mode,
        depends_on=depends_on,
        priority=priority,
        ttl=ttl,
        wait=True,
        timeout=timeout,
    )

    stop_event = threading.Event()

    def _heartbeat_loop():
        while not stop_event.wait(5.0):
            try:
                client.heartbeat(l.id)
            except Exception:
                pass

    hb_thread = threading.Thread(target=_heartbeat_loop, daemon=True)
    hb_thread.start()

    try:
        yield l
    finally:
        stop_event.set()
        hb_thread.join(timeout=1.0)
        try:
            client.release(l.id)
        except Exception as e:
            sys.stderr.write(f"Warning: Failed to release Nomos lease {l.id}: {e}\n")
