use crate::client::NomosClient;
use crate::ipc::default_socket_path;
use nomos_core::budget::parse_bytes_string;
use nomos_core::lease::{DeviceType, LeaseRequest, NetworkMode, Priority};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::process::Command;
use tracing::{error, info};

pub struct RunArgs {
    pub cpu: f64,
    pub memory: String,
    pub scratch: Option<String>,
    pub devices: Vec<String>,
    pub priority: u32,
    pub ttl: u64,
    pub worker_id: Option<String>,
    pub depends_on: Vec<String>,
    pub socket_path: Option<PathBuf>,
    pub command: Vec<String>,
}

pub async fn execute_runner(args: RunArgs) -> Result<ExitStatus, anyhow::Error> {
    if args.command.is_empty() {
        return Err(anyhow::anyhow!("No command specified to execute"));
    }

    let sock = args.socket_path.unwrap_or_else(default_socket_path);
    let mut client = NomosClient::connect(&sock).await
        .map_err(|e| anyhow::anyhow!("Cannot connect to Nomos arbiter at {:?}: {}. Is 'nomos daemon' running?", sock, e))?;

    let mem_bytes = parse_bytes_string(&args.memory)?;
    let scratch_bytes = if let Some(s) = args.scratch {
        parse_bytes_string(&s)?
    } else {
        0
    };

    let devices = args.devices.into_iter().map(|d| {
        if d.to_lowercase().contains("npu") {
            DeviceType::Npu(d)
        } else {
            DeviceType::Gpu(d)
        }
    }).collect();

    let worker_name = args.worker_id.unwrap_or_else(|| {
        let cmd_base = std::path::Path::new(&args.command[0])
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("worker");
        format!("cli-{}", cmd_base)
    });

    let lease_req = LeaseRequest {
        worker_id: worker_name.clone(),
        tenant: "default".into(),
        req_cpu: args.cpu,
        req_memory_bytes: mem_bytes,
        req_scratch_bytes: scratch_bytes,
        devices,
        network_mode: NetworkMode::Isolated,
        network_bandwidth_mbps: None,
        estimated_seconds: None,
        deadline: None,
        depends_on: args.depends_on,
        priority: Priority::from_u32(args.priority),
        ttl_seconds: args.ttl,
    };

    info!("Worker '{}' requesting lease for command {:?} (CPU: {:.1}, RAM: {})...", worker_name, args.command, args.cpu, args.memory);
    let lease = client.acquire_lease(lease_req).await?;
    info!("Lease {} GRANTED to worker '{}'! Starting child process...", lease.id, worker_name);

    // Spawn heartbeat task
    let lease_id = lease.id.clone();
    let hb_sock = sock.clone();
    let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();

    let hb_handle = tokio::spawn(async move {
        if let Ok(mut hb_client) = NomosClient::connect(hb_sock).await {
            let mut interval = tokio::time::interval(Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let _ = hb_client.heartbeat(&lease_id).await;
                    }
                    _ = &mut cancel_rx => {
                        break;
                    }
                }
            }
        }
    });

    // Execute child process
    let mut child = Command::new(&args.command[0]);
    if args.command.len() > 1 {
        child.args(&args.command[1..]);
    }

    let status = child.status().await;

    // Stop heartbeat task
    let _ = cancel_tx.send(());
    let _ = hb_handle.await;

    // Release lease
    info!("Process completed, releasing Lease {}...", lease.id);
    if let Err(e) = client.release_lease(&lease.id).await {
        error!("Failed to release lease {}: {}", lease.id, e);
    } else {
        info!("Lease {} cleanly released.", lease.id);
    }

    match status {
        Ok(s) => Ok(s),
        Err(e) => Err(anyhow::anyhow!("Failed to run command {:?}: {}", args.command, e)),
    }
}
