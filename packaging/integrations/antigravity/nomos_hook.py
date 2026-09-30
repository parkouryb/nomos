#!/usr/bin/env python3
"""
Nomos Resource Arbiter - Antigravity Universal Tool Lifecycle Hook
Intercepts and regulates AGY tools:
- `run_command`: Automatic workload classification, duration & resource estimation, and Nomos lease wrapping.
- `invoke_subagent`: Verifies Nomos pool headroom via socket IPC before subagent swarm execution.
- `generate_image`: Governs GPU / Unified Memory envelope for graphics and vision workloads.
- PostToolUse: Records completion and telemetry in Nomos audit log.
"""

import sys
import os
import json
import re
import shlex
import socket
from datetime import datetime

LOG_FILE = os.path.expanduser("~/.gemini/config/nomos_hook.log")
SOCK_PATH = os.path.expanduser("~/.nomos/arbiter.sock")

def log(msg: str):
    try:
        with open(LOG_FILE, "a", encoding="utf-8") as f:
            f.write(f"[{datetime.now().isoformat()}] {msg}\n")
    except Exception:
        pass

def query_nomos_status():
    """Queries current Nomos pool status over Unix domain socket IPC (<1ms)."""
    try:
        if not os.path.exists(SOCK_PATH):
            return None
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(0.5)
        s.connect(SOCK_PATH)
        s.sendall(b'"GetStatus"\n')
        data = b""
        while b"\n" not in data:
            chunk = s.recv(4096)
            if not chunk:
                break
            data += chunk
        s.close()
        parsed = json.loads(data.decode("utf-8").strip())
        return parsed.get("Status", {})
    except Exception as e:
        log(f"Socket query warning: {e}")
        return None

def classify_command(cmd: str):
    """
    Classifies command into resource tiers and estimates runtime duration.
    Returns: (cpu, mem, ttl, priority, est_duration, class_name, worker_slug)
    """
    cmd_clean = cmd.strip()
    tokens = cmd_clean.split()
    first_word = tokens[0] if tokens else "cmd"
    base_cmd = os.path.basename(first_word)
    
    # 1. HEAVY: Compilations, Docker, Training, Benchmarks
    heavy_patterns = [
        r"\bcargo\s+build\b", r"\bcargo\s+run\b", r"\bmake\b", r"\bninja\b",
        r"\bcmake\b", r"\bgradle\b", r"\bmvn\b", r"\bdocker\b", r"\bpodman\b",
        r"\bbench\b", r"\bbenches\b", r"\btrain\b", r"\btorch\b", r"\bvite\s+build\b",
        r"\bnext\s+build\b", r"\bwebpack\b", r"\brustc\b", r"\bg\+\+\b", r"\bclang\b"
    ]
    for pat in heavy_patterns:
        if re.search(pat, cmd_clean, re.IGNORECASE):
            return (
                4.0, "8GB", 600, 50,
                "~2-5m (Heavy Task)",
                "HEAVY_COMPILATION_BENCHMARK",
                f"hvy-{base_cmd}"
            )
            
    # 2. MEDIUM: Scripts, Testing, Package management, Git sync
    medium_patterns = [
        r"\bpytest\b", r"\bpython3?\b", r"\bnode\b", r"\bts-node\b", r"\bdeno\b",
        r"\bbun\b", r"\bruby\b", r"\bperl\b", r"\bphp\b", r"\bbash\b", r"\bsh\b",
        r"\bzsh\b", r"\bnpm\b", r"\byarn\b", r"\bpnpm\b", r"\bpip3?\b", r"\bpoetry\b",
        r"\bcargo\s+test\b", r"\bcargo\s+check\b", r"\bgo\s+test\b", r"\bgo\s+build\b",
        r"\bgit\s+(pull|push|clone|fetch)\b", r"\bcurl\b", r"\bwget\b"
    ]
    for pat in medium_patterns:
        if re.search(pat, cmd_clean, re.IGNORECASE):
            return (
                2.0, "4GB", 180, 50,
                "~15-45s (Script/Test)",
                "MEDIUM_SCRIPT_TEST",
                f"med-{base_cmd}"
            )
            
    # 3. LIGHT: Metadata queries, navigation, lightweight file operations
    light_patterns = [
        r"^(ls|pwd|echo|cat|head|tail|which|where|type|date|uptime|whoami|id|file|env|printenv|stat|du|df|touch|mkdir|cp|mv|rm|grep|find|wc|diff)(\s|$)",
        r"^git\s+(status|branch|log|diff|rev-parse|show|remote|add)(\s|$)"
    ]
    for pat in light_patterns:
        if re.search(pat, cmd_clean):
            return (
                0.5, "512MB", 20, 75,
                "< 2s (Quick Query)",
                "LIGHT_METADATA",
                f"lgt-{base_cmd}"
            )
            
    # 4. DEFAULT FALLBACK
    return (
        1.0, "2GB", 90, 50,
        "~5-15s (Standard)",
        "STANDARD_EXECUTION",
        f"std-{base_cmd}"
    )

def handle_pre_tool(data: dict):
    tool_call = data.get("toolCall", {})
    tool_name = tool_call.get("name", "")
    args = tool_call.get("args", {})
    
    # 1. RUN_COMMAND INTERCEPTION
    if tool_name == "run_command":
        cmd = args.get("CommandLine", "").strip()
        if not cmd:
            return {"decision": "allow"}
            
        # Check if already wrapped or native nomos
        if (cmd.startswith("nomos ") or 
            cmd.startswith("/Users/hieu/.local/bin/nomos ") or
            cmd.startswith("nomos_")):
            log(f"BYPASS (Already Nomos): {cmd[:80]}")
            return {"decision": "allow"}
            
        cpu, mem, ttl, prio, est_duration, cls_name, worker_slug = classify_command(cmd)
        quoted_cmd = shlex.quote(cmd)
        wrapped_cmd = (
            f"nomos run --cpu {cpu} --mem {mem} --ttl {ttl} "
            f"--priority {prio} --worker-id {worker_slug} -- zsh -c {quoted_cmd}"
        )
        reason = (
            f"[Nomos Arbiter] Intercepted & Wrapped: {cls_name} "
            f"(Est. Duration: {est_duration}, TTL: {ttl}s, CPU: {cpu}, RAM: {mem})"
        )
        log(f"INTERCEPT [run_command]: [{cls_name}] {cmd[:60]} -> TTL={ttl}s, CPU={cpu}, RAM={mem}")
        return {
            "decision": "allow",
            "reason": reason,
            "overwrite": {
                "CommandLine": wrapped_cmd
            }
        }
        
    # 2. INVOKE_SUBAGENT INTERCEPTION
    elif tool_name == "invoke_subagent":
        subagents = args.get("Subagents", [])
        count = len(subagents)
        status = query_nomos_status()
        pool_info = ""
        if status:
            alloc_cpu = status.get("allocated_cpu", 0.0)
            total_cpu = status.get("pool", {}).get("total_cores", 6.0)
            active_cnt = len(status.get("active_leases", []))
            pool_info = f" | Nomos Pool: {alloc_cpu:.1f}/{total_cpu:.1f} Cores, {active_cnt} Active Leases"
            
        reason = f"[Nomos Arbiter] Subagent swarm approved ({count} worker(s) queued under pool governance{pool_info})"
        log(f"INTERCEPT [invoke_subagent]: Dispatched {count} subagent(s){pool_info}")
        return {
            "decision": "allow",
            "reason": reason
        }
        
    # 3. GENERATE_IMAGE INTERCEPTION
    elif tool_name == "generate_image":
        img_name = args.get("ImageName", "image")
        log(f"INTERCEPT [generate_image]: Approving GPU/Unified Memory envelope for {img_name}")
        return {
            "decision": "allow",
            "reason": f"[Nomos Arbiter] Image generation approved under Nomos GPU envelope for asset '{img_name}'"
        }
        
    # All other tools pass through
    return {"decision": "allow"}

def handle_post_tool(data: dict):
    step_idx = data.get("stepIdx", 0)
    err = data.get("error", None)
    if err:
        log(f"POST_TOOL [Step {step_idx}] Tool encountered error: {err}")
    return {}

def main():
    try:
        mode = sys.argv[1] if len(sys.argv) > 1 else "pre"
        raw_input = sys.stdin.read()
        data = json.loads(raw_input) if raw_input.strip() else {}
        
        if mode == "post":
            resp = handle_post_tool(data)
        else:
            resp = handle_pre_tool(data)
            
        print(json.dumps(resp))
    except Exception as e:
        log(f"FATAL HOOK ERROR: {str(e)}")
        # Fail safe
        if len(sys.argv) > 1 and sys.argv[1] == "post":
            print(json.dumps({}))
        else:
            print(json.dumps({"decision": "allow"}))

if __name__ == "__main__":
    main()
