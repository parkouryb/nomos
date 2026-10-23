use crate::client::NomosClient;
use crate::ipc::default_socket_path;
use nomos_core::budget::{format_bytes, parse_bytes_string};
use nomos_core::lease::{LeaseRequest, NetworkMode, Priority};
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
            "description": "Execute a command with kernel-enforced CPU/RAM limits on Linux. Explicit host networking is required; GPU/device, scratch and network isolation are unsupported.",
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
                        "description": "Unsupported; any scratch request is rejected"
                    },
                    "devices": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Unsupported; nonempty device requests are rejected"
                    },
                    "network": {
                        "type": "string",
                        "enum": ["host"],
                        "description": "Required: host networking, without network isolation"
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
                "required": ["command", "network"]
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
            "description": "Query the historical resource audit ledger: total jobs executed, cumulative CPU core-hours, GPU hours, peak memory recorded, and recent job executions across a time retention window (default: 30 days).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "days": {
                        "type": "integer",
                        "description": "Time window in days to aggregate accounting metrics (default: 30)"
                    },
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

async fn handle_nomos_run(sock_path:&std::path::Path,arguments:Value)->Result<(String,bool),String> {
    let command_str=arguments.get("command").and_then(Value::as_str).ok_or("Missing command")?;
    let network=arguments.get("network").and_then(Value::as_str).unwrap_or("isolated");
    if network!="host" {return Err("Network isolation is not implemented; explicitly select host networking".into());}
    if arguments.get("scratch").is_some() || arguments.get("devices").and_then(Value::as_array).is_some_and(|a|!a.is_empty()) {return Err("Device and scratch enforcement is not implemented".into());}
    let ttl=arguments.get("timeout_seconds").and_then(Value::as_u64).unwrap_or(120);
    let mut command=Command::new(std::env::current_exe().map_err(|e|e.to_string())?);
    command.arg("run").arg("--socket").arg(sock_path)
        .args(["--network","host","--cpu",&arguments.get("cpu").and_then(Value::as_f64).unwrap_or(1.0).to_string(),
            "--mem",arguments.get("memory").and_then(Value::as_str).unwrap_or("1GB"),"--ttl",&ttl.to_string(),
            "--priority",&arguments.get("priority").and_then(Value::as_u64).unwrap_or(50).to_string(),"--","sh","-c",command_str]);
    if let Some(dir)=arguments.get("working_dir").and_then(Value::as_str){command.current_dir(dir);}
    command.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    let start=Instant::now();
    let output=tokio::time::timeout(Duration::from_secs(ttl.saturating_add(30)),command.output()).await
        .map_err(|_|"Runner did not finish cleanup before the MCP deadline; do not assume resources were released".to_owned())?
        .map_err(|e|e.to_string())?;
    let report=json!({"exit_code":output.status.code(),"duration_seconds":start.elapsed().as_secs_f64(),
        "stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),
        "resource_usage":"Consult measured Nomos accounting; reservation sizes are not measurements"});
    Ok((report.to_string(),!output.status.success()))
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
    let days = arguments.get("days").and_then(|v| v.as_u64()).map(|d| d as u32).or(Some(30));

    let mut client = NomosClient::connect(sock_path).await
        .map_err(|e| format!("Cannot connect to Nomos Arbiter at {:?}: {}. Please start daemon with 'nomos daemon'.", sock_path, e))?;

    let (summary, recent) = client.get_accounting_window(limit, days).await
        .map_err(|e| format!("Failed to query accounting ledger: {}", e))?;

    let days_label = days.map(|d| format!(" (Last {} Days)", d)).unwrap_or_default();
    let mut report = format!(
        "=== NOMOS RESOURCE ACCOUNTING LEDGER{} ===\n\
         Total Jobs Tracked:       {}\n\
         Cumulative CPU Core-Hours: {:.4} hrs\n\
         Cumulative GPU Hours:      {:.4} hrs\n\
         Peak Memory Recorded:      {}\n\
         ------------------------------------\n\
         Recent Completed Jobs (Last {}):\n",
        days_label,
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
