use crate::client::NomosClient;
use crate::ipc::default_socket_path;
use nomos_core::budget::{format_bytes, parse_bytes_string};
use nomos_core::lease::{DeviceType, LeaseRequest, NetworkMode, Priority};
use nomos_sys::probe::probe_host;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub fn get_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "nomos_run",
            "description": "Execute a shell command bounded by Nomos resource arbiter (vCPU, RAM, GPU/NPU, network isolation, priority). Prevents runaway tasks from freezing the host system.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute (e.g. 'python3 train.py', 'ffmpeg -i in.mov out.mp4', 'cargo test')"
                    },
                    "cpu": {
                        "type": "number",
                        "description": "Allocated vCPU cores (e.g. 1.0, 2.0, 4.0). Default: 1.0"
                    },
                    "memory": {
                        "type": "string",
                        "description": "Allocated RAM with unit (e.g. '512MB', '2GB', '8GB'). Default: '1GB'"
                    },
                    "scratch": {
                        "type": "string",
                        "description": "Allocated temporary scratch disk space with unit (e.g. '2GB')"
                    },
                    "devices": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Hardware accelerators required (e.g. ['gpu'], ['npu'])"
                    },
                    "network": {
                        "type": "string",
                        "enum": ["isolated", "none", "host"],
                        "description": "Network egress mode: 'isolated' (LAN and loopback allowed, WAN blocked), 'none' (air-gapped), 'host' (unrestricted). Default: 'isolated'"
                    },
                    "priority": {
                        "type": "integer",
                        "description": "Execution priority (10=Low, 50=Normal, 75=High, 100=Critical). Default: 50"
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "description": "Execution timeout in seconds before force-terminating. Default: 120"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Working directory to execute command within"
                    }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "nomos_probe",
            "description": "Probe physical host hardware capacity and accelerators: total CPU cores, RAM, swap, storage, GPU accelerators, and NPU accelerators.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "nomos_status",
            "description": "Query current Nomos arbiter status: pool headroom, current CPU and RAM allocation percentages, active leases, and queued waiting requests.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }),
        json!({
            "name": "nomos_accounting",
            "description": "Query the historical resource audit ledger: total jobs executed, cumulative CPU core-hours, GPU hours, peak memory recorded, and recent job executions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of recent records to return. Default: 20"
                    }
                }
            }
        }),
        json!({
            "name": "nomos_acquire",
            "description": "Explicitly acquire a Nomos resource lease for long-running or multi-stage tasks. Must be manually released with nomos_release when done.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "worker_id": {
                        "type": "string",
                        "description": "Unique identifier for the worker or pipeline step"
                    },
                    "cpu": {
                        "type": "number",
                        "description": "Allocated vCPU cores. Default: 1.0"
                    },
                    "memory": {
                        "type": "string",
                        "description": "Allocated RAM (e.g. '2GB'). Default: '1GB'"
                    },
                    "priority": {
                        "type": "integer",
                        "description": "Priority (10=Low, 50=Normal, 75=High, 100=Critical). Default: 50"
                    },
                    "ttl_seconds": {
                        "type": "integer",
                        "description": "Lease time-to-live before Dead-Man auto-reclaim. Default: 120"
                    }
                },
                "required": ["worker_id"]
            }
        }),
        json!({
            "name": "nomos_release",
            "description": "Release an explicitly acquired Nomos resource lease by its lease ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "lease_id": {
                        "type": "string",
                        "description": "The unique lease ID to release"
                    }
                },
                "required": ["lease_id"]
            }
        })
    ]
}

async fn handle_nomos_run(
    sock_path: &std::path::Path,
    arguments: Value,
) -> Result<(String, bool), String> {
    let command_str = arguments
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'command'".to_string())?;

    let cpu = arguments
        .get("cpu")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);

    let mem_str = arguments
        .get("memory")
        .and_then(|v| v.as_str())
        .unwrap_or("1GB");

    let scratch_str = arguments
        .get("scratch")
        .and_then(|v| v.as_str());

    let priority = arguments
        .get("priority")
        .and_then(|v| v.as_u64())
        .map(|p| p as u32)
        .unwrap_or(50);

    let timeout_secs = arguments
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(120);

    let working_dir = arguments
        .get("working_dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let net_mode_str = arguments
        .get("network")
        .and_then(|v| v.as_str())
        .unwrap_or("isolated");

    let network_mode = match net_mode_str.to_lowercase().as_str() {
        "none" => NetworkMode::None,
        "host" => NetworkMode::Host,
        _ => NetworkMode::Isolated,
    };

    let devices: Vec<DeviceType> = arguments
        .get("devices")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .map(|d| {
                    if d.to_lowercase().contains("npu") {
                        DeviceType::Npu(d.to_string())
                    } else {
                        DeviceType::Gpu(d.to_string())
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let mem_bytes = parse_bytes_string(mem_str)
        .map_err(|e| format!("Invalid memory string '{}': {}", mem_str, e))?;

    let scratch_bytes = if let Some(s) = scratch_str {
        parse_bytes_string(s).map_err(|e| format!("Invalid scratch string '{}': {}", s, e))?
    } else {
        0
    };

    let mut client = NomosClient::connect(sock_path)
        .await
        .map_err(|e| format!("Cannot connect to Nomos Arbiter daemon at {:?}: {}. Please start daemon with 'nomos daemon'.", sock_path, e))?;

    let worker_id = format!("mcp-{}", &uuid::Uuid::new_v4().to_string()[..8]);

    let req = LeaseRequest {
        worker_id: worker_id.clone(),
        tenant: "mcp".into(),
        req_cpu: cpu,
        req_memory_bytes: mem_bytes,
        req_scratch_bytes: scratch_bytes,
        devices,
        network_mode,
        network_bandwidth_mbps: None,
        estimated_seconds: Some(timeout_secs as f64),
        deadline: None,
        depends_on: vec![],
        priority: Priority::from_u32(priority),
        ttl_seconds: timeout_secs + 30,
    };

    eprintln!("[nomos-mcp] Acquiring lease for worker '{}' (command: '{}')...", worker_id, command_str);
    let lease = client.acquire_lease_wait(req, Duration::from_secs(60)).await
        .map_err(|e| format!("Failed to acquire Nomos lease: {}", e))?;

    eprintln!("[nomos-mcp] Lease '{}' granted. Spawning process...", lease.id);

    // Heartbeat background task
    let lease_id = lease.id.clone();
    let hb_sock = sock_path.to_path_buf();
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();

    let hb_lease_id = lease_id.clone();
    let hb_handle = tokio::spawn(async move {
        if let Ok(mut hb_client) = NomosClient::connect(&hb_sock).await {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let _ = hb_client.heartbeat(&hb_lease_id).await;
                    }
                    _ = &mut cancel_rx => {
                        break;
                    }
                }
            }
        }
    });

    let start_time = Instant::now();

    // Spawn process via sh -c
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command_str);
    if let Some(ref dir) = working_dir {
        cmd.current_dir(dir);
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = cancel_tx.send(());
            let _ = hb_handle.await;
            let _ = client.release_lease(&lease_id).await;
            return Err(format!("Failed to spawn process: {}", e));
        }
    };

    if let Some(pid) = child.id() {
        let cgroups = nomos_sys::cgroup::CgroupController::new();
        if let Err(e) = cgroups.attach_pid_with_mode(&lease.id, network_mode, pid) {
            eprintln!("[nomos-mcp] Could not attach PID {} to cgroup (mode: {}): {}", pid, network_mode, e);
        } else {
            eprintln!("[nomos-mcp] Attached PID {} to cgroup slice {} (network: {})", pid, lease.id, network_mode);
        }
    }

    // Wait with timeout
    let timeout_dur = Duration::from_secs(timeout_secs);
    let wait_res = tokio::time::timeout(timeout_dur, child.wait_with_output()).await;

    let _ = cancel_tx.send(());
    let _ = hb_handle.await;

    // Release lease
    let _ = client.release_lease(&lease_id).await;

    let elapsed = start_time.elapsed();

    match wait_res {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let is_error = !output.status.success();

            let stdout_str = String::from_utf8_lossy(&output.stdout);
            let stderr_str = String::from_utf8_lossy(&output.stderr);

            let max_bytes = 512 * 1024; // 512 KB
            let truncated_stdout = if stdout_str.len() > max_bytes {
                format!("{}\n[... stdout truncated after 512 KB ...]", &stdout_str[..max_bytes])
            } else {
                stdout_str.into_owned()
            };

            let truncated_stderr = if stderr_str.len() > max_bytes {
                format!("{}\n[... stderr truncated after 512 KB ...]", &stderr_str[..max_bytes])
            } else {
                stderr_str.into_owned()
            };

            let report = format!(
                "[Nomos Execution Report]\n\
                 Command: {}\n\
                 Status: {}\n\
                 Exit Code: {}\n\
                 Duration: {:.3}s\n\
                 Lease ID: {}\n\
                 Allocated: {:.1} vCPU | {} RAM | Network: {}\n\n\
                 === STDOUT ===\n\
                 {}\n\n\
                 === STDERR ===\n\
                 {}",
                command_str,
                if is_error { "FAILED" } else { "SUCCESS" },
                exit_code,
                elapsed.as_secs_f64(),
                lease_id,
                cpu,
                mem_str,
                network_mode,
                if truncated_stdout.trim().is_empty() { "(empty)" } else { truncated_stdout.trim() },
                if truncated_stderr.trim().is_empty() { "(empty)" } else { truncated_stderr.trim() }
            );

            Ok((report, is_error))
        }
        Ok(Err(e)) => {
            Err(format!("Process execution error: {}", e))
        }
        Err(_) => {
            // Timed out
            Err(format!("Command timed out after {} seconds. Lease {} auto-reclaimed.", timeout_secs, lease_id))
        }
    }
}

fn handle_nomos_probe() -> Result<(String, bool), String> {
    let hw = probe_host();
    let gpu_str = if hw.gpu_details.is_empty() {
        "  (none)".to_string()
    } else {
        hw.gpu_details.iter().map(|d| format!("  - {}", d)).collect::<Vec<_>>().join("\n")
    };
    let npu_str = if hw.npu_details.is_empty() {
        "  (none)".to_string()
    } else {
        hw.npu_details.iter().map(|d| format!("  - {}", d)).collect::<Vec<_>>().join("\n")
    };

    let report = format!(
        "=== NOMOS HOST HARDWARE TELEMETRY ===\n\
         Physical CPU Cores:      {}\n\
         Physical RAM:            {}\n\
         Physical Swap:           {}\n\
         Total Storage Space:     {}\n\
         ------------------------------------\n\
         GPU Accelerator:         {}\n\
         {}\n\
         NPU Accelerator:         {}\n\
         {}\n\
         ====================================",
        hw.resources.total_cores,
        format_bytes(hw.resources.total_memory_bytes),
        format_bytes(hw.resources.total_swap_bytes),
        format_bytes(hw.resources.total_storage_bytes),
        if hw.has_gpu { "DETECTED" } else { "None" },
        gpu_str,
        if hw.has_npu { "DETECTED" } else { "None" },
        npu_str
    );
    Ok((report, false))
}

async fn handle_nomos_status(sock_path: &std::path::Path) -> Result<(String, bool), String> {
    let mut client = NomosClient::connect(sock_path).await
        .map_err(|e| format!("Cannot connect to Nomos Arbiter at {:?}: {}. Please start daemon with 'nomos daemon'.", sock_path, e))?;

    let status = client.get_status().await
        .map_err(|e| format!("Failed to query status from Arbiter: {}", e))?;

    let alloc_cpu = if status.allocated_cpu.abs() < 1e-6 { 0.0 } else { status.allocated_cpu };
    let cpu_pct = if status.pool.total_cores > 0.0 {
        alloc_cpu / status.pool.total_cores * 100.0
    } else {
        0.0
    };
    let mem_pct = if status.pool.total_memory_bytes > 0 {
        status.allocated_memory_bytes as f64 / status.pool.total_memory_bytes as f64 * 100.0
    } else {
        0.0
    };

    let mut report = format!(
        "=== NOMOS RESOURCE ARBITER STATUS ===\n\
         CPU ALLOCATION:    {:.1} / {:.1} Cores ({:.1}% of Nomos Pool)\n\
         RAM ALLOCATION:    {} / {} ({:.1}% of Nomos Pool)\n\
         ACTIVE LEASES:     {} running\n\
         QUEUED LEASES:     {} waiting in line\n\
         ------------------------------------\n",
        alloc_cpu, status.pool.total_cores, cpu_pct,
        format_bytes(status.allocated_memory_bytes),
        format_bytes(status.pool.total_memory_bytes),
        mem_pct,
        status.active_leases.len(),
        status.queued_leases.len()
    );

    if status.active_leases.is_empty() {
        report.push_str("Active Leases: None\n");
    } else {
        report.push_str("Active Leases:\n");
        for l in &status.active_leases {
            report.push_str(&format!(
                " - [{}] Worker: {:<16} vCPU: {:.1}  RAM: {}  Priority: {:?}  State: {:?}\n",
                l.id, l.request.worker_id, l.request.req_cpu, format_bytes(l.request.req_memory_bytes), l.request.priority, l.state
            ));
        }
    }

    if !status.queued_leases.is_empty() {
        report.push_str("\nQueued Leases:\n");
        for (i, q) in status.queued_leases.iter().enumerate() {
            report.push_str(&format!(
                " #{} - Worker: {} (Needs {:.1} vCPU, {})\n",
                i + 1, q.request.worker_id, q.request.req_cpu, format_bytes(q.request.req_memory_bytes)
            ));
        }
    }

    Ok((report, false))
}

async fn handle_nomos_accounting(sock_path: &std::path::Path, arguments: Value) -> Result<(String, bool), String> {
    let limit = arguments.get("limit").and_then(|v| v.as_u64()).map(|l| l as usize).unwrap_or(20);

    let mut client = NomosClient::connect(sock_path).await
        .map_err(|e| format!("Cannot connect to Nomos Arbiter at {:?}: {}. Please start daemon with 'nomos daemon'.", sock_path, e))?;

    let (summary, recent) = client.get_accounting(limit).await
        .map_err(|e| format!("Failed to query accounting ledger: {}", e))?;

    let mut report = format!(
        "=== NOMOS RESOURCE ACCOUNTING LEDGER ===\n\
         Total Jobs Tracked:       {}\n\
         Cumulative CPU Core-Hours: {:.4} hrs\n\
         Cumulative GPU Hours:      {:.4} hrs\n\
         Peak Memory Recorded:      {}\n\
         ------------------------------------\n\
         Recent Completed Jobs (Last {}):\n",
        summary.total_records,
        summary.total_cpu_hours,
        summary.total_gpu_hours,
        format_bytes(summary.peak_memory_seen_bytes),
        recent.len()
    );

    for r in recent {
        report.push_str(&format!(
            " - [{}] Worker: {:<16} Duration: {:.1}s  CPU-sec: {:.1}  Peak RAM: {}\n",
            r.lease_id, r.worker_id, r.duration_seconds, r.cpu_core_seconds, format_bytes(r.peak_memory_bytes)
        ));
    }

    Ok((report, false))
}

async fn handle_nomos_acquire(sock_path: &std::path::Path, arguments: Value) -> Result<(String, bool), String> {
    let worker_id = arguments
        .get("worker_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'worker_id'".to_string())?;

    let cpu = arguments.get("cpu").and_then(|v| v.as_f64()).unwrap_or(1.0);
    let mem_str = arguments.get("memory").and_then(|v| v.as_str()).unwrap_or("1GB");
    let priority = arguments.get("priority").and_then(|v| v.as_u64()).map(|p| p as u32).unwrap_or(50);
    let ttl_seconds = arguments.get("ttl_seconds").and_then(|v| v.as_u64()).unwrap_or(120);

    let mem_bytes = parse_bytes_string(mem_str)
        .map_err(|e| format!("Invalid memory string '{}': {}", mem_str, e))?;

    let mut client = NomosClient::connect(sock_path).await
        .map_err(|e| format!("Cannot connect to Nomos Arbiter at {:?}: {}", sock_path, e))?;

    let req = LeaseRequest {
        worker_id: worker_id.to_string(),
        tenant: "mcp".into(),
        req_cpu: cpu,
        req_memory_bytes: mem_bytes,
        req_scratch_bytes: 0,
        devices: vec![],
        network_mode: NetworkMode::Host,
        network_bandwidth_mbps: None,
        estimated_seconds: Some(ttl_seconds as f64),
        deadline: None,
        depends_on: vec![],
        priority: Priority::from_u32(priority),
        ttl_seconds,
    };

    let lease = client.acquire_lease_wait(req, Duration::from_secs(60)).await
        .map_err(|e| format!("Failed to acquire lease: {}", e))?;

    let expires_str = lease.expires_at.map(|t| t.to_rfc3339()).unwrap_or_else(|| "N/A".to_string());
    let report = format!(
        "Lease Granted Successfully!\n\
         Lease ID: {}\n\
         Worker: {}\n\
         Allocated: {:.1} vCPU | {}\n\
         TTL: {}s\n\
         Expires At: {}\n\n\
         Remember to call 'nomos_release' with lease_id '{}' when your workflow finishes.",
        lease.id, lease.request.worker_id, lease.request.req_cpu, format_bytes(lease.request.req_memory_bytes),
        lease.request.ttl_seconds, expires_str, lease.id
    );

    Ok((report, false))
}

async fn handle_nomos_release(sock_path: &std::path::Path, arguments: Value) -> Result<(String, bool), String> {
    let lease_id = arguments
        .get("lease_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'lease_id'".to_string())?;

    let mut client = NomosClient::connect(sock_path).await
        .map_err(|e| format!("Cannot connect to Nomos Arbiter at {:?}: {}", sock_path, e))?;

    client.release_lease(lease_id).await
        .map_err(|e| format!("Failed to release lease '{}': {}", lease_id, e))?;

    Ok((format!("Lease '{}' was successfully released back to the Nomos pool.", lease_id), false))
}

pub async fn run_mcp_server(socket_path: Option<PathBuf>) -> Result<(), anyhow::Error> {
    let sock = socket_path.unwrap_or_else(default_socket_path);
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    eprintln!("[nomos-mcp] Nomos MCP Server started on stdio. Using socket: {:?}", sock);

    while let Ok(Some(line)) = reader.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("[nomos-mcp] Failed to parse JSON-RPC request: {}", e);
                let err_resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                        data: None,
                    }),
                };
                let out = serde_json::to_string(&err_resp)?;
                stdout.write_all(out.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
        };

        let req_id = request.id.clone();
        let method = request.method.as_str();

        match method {
            "initialize" => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req_id,
                    result: Some(json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "nomos",
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    })),
                    error: None,
                };
                write_response(&mut stdout, &resp).await?;
            }

            "notifications/initialized" => {
                eprintln!("[nomos-mcp] Client handshake complete (notifications/initialized received).");
            }

            "ping" => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req_id,
                    result: Some(json!({})),
                    error: None,
                };
                write_response(&mut stdout, &resp).await?;
            }

            "tools/list" => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req_id,
                    result: Some(json!({
                        "tools": get_tool_definitions()
                    })),
                    error: None,
                };
                write_response(&mut stdout, &resp).await?;
            }

            "tools/call" => {
                let params = request.params.unwrap_or(json!({}));
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                let (text_content, is_error) = match tool_name {
                    "nomos_run" => match handle_nomos_run(&sock, arguments).await {
                        Ok((txt, err)) => (txt, err),
                        Err(e) => (format!("Error executing command: {}", e), true),
                    },
                    "nomos_probe" => match handle_nomos_probe() {
                        Ok((txt, err)) => (txt, err),
                        Err(e) => (format!("Error probing host: {}", e), true),
                    },
                    "nomos_status" => match handle_nomos_status(&sock).await {
                        Ok((txt, err)) => (txt, err),
                        Err(e) => (format!("Error querying status: {}", e), true),
                    },
                    "nomos_accounting" => match handle_nomos_accounting(&sock, arguments).await {
                        Ok((txt, err)) => (txt, err),
                        Err(e) => (format!("Error querying accounting: {}", e), true),
                    },
                    "nomos_acquire" => match handle_nomos_acquire(&sock, arguments).await {
                        Ok((txt, err)) => (txt, err),
                        Err(e) => (format!("Error acquiring lease: {}", e), true),
                    },
                    "nomos_release" => match handle_nomos_release(&sock, arguments).await {
                        Ok((txt, err)) => (txt, err),
                        Err(e) => (format!("Error releasing lease: {}", e), true),
                    },
                    unknown => (format!("Unknown tool: {}", unknown), true),
                };

                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req_id,
                    result: Some(json!({
                        "content": [
                            {
                                "type": "text",
                                "text": text_content
                            }
                        ],
                        "isError": is_error
                    })),
                    error: None,
                };
                write_response(&mut stdout, &resp).await?;
            }

            "resources/list" => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req_id,
                    result: Some(json!({ "resources": [] })),
                    error: None,
                };
                write_response(&mut stdout, &resp).await?;
            }

            "prompts/list" => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req_id,
                    result: Some(json!({ "prompts": [] })),
                    error: None,
                };
                write_response(&mut stdout, &resp).await?;
            }

            other => {
                if req_id.is_some() {
                    let resp = JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req_id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32601,
                            message: format!("Method '{}' not found", other),
                            data: None,
                        }),
                    };
                    write_response(&mut stdout, &resp).await?;
                }
            }
        }
    }

    eprintln!("[nomos-mcp] Stdio stream closed. Nomos MCP Server exiting.");
    Ok(())
}

async fn write_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    resp: &JsonRpcResponse,
) -> Result<(), anyhow::Error> {
    let out = serde_json::to_string(resp)?;
    writer.write_all(out.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definitions() {
        let tools = get_tool_definitions();
        assert_eq!(tools.len(), 6);
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"nomos_run"));
        assert!(names.contains(&"nomos_probe"));
        assert!(names.contains(&"nomos_status"));
        assert!(names.contains(&"nomos_accounting"));
        assert!(names.contains(&"nomos_acquire"));
        assert!(names.contains(&"nomos_release"));
    }

    #[test]
    fn test_handle_nomos_probe() {
        let (report, is_error) = handle_nomos_probe().unwrap();
        assert!(!is_error);
        assert!(report.contains("NOMOS HOST HARDWARE TELEMETRY"));
        assert!(report.contains("Physical CPU Cores"));
    }

    #[test]
    fn test_jsonrpc_request_parsing() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.id.unwrap(), 1);
        assert_eq!(req.method, "initialize");
    }
}
