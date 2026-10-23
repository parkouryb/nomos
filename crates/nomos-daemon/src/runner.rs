use crate::client::NomosClient;
use crate::ipc::default_socket_path;
use nomos_core::budget::parse_bytes_string;
use nomos_core::lease::{DeviceType, LeaseRequest, NetworkMode, Priority};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;
use tokio::process::Command;
use tracing::info;

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
    pub network_mode: NetworkMode,
}

pub async fn execute_runner(args: RunArgs) -> Result<ExitStatus, anyhow::Error> {
    if args.command.is_empty() {
        return Err(anyhow::anyhow!("No command specified to execute"));
    }

    #[cfg(target_os = "linux")]
    if !nix::unistd::geteuid().is_root() && std::env::var_os("NOMOS_RUN_SCOPED").is_none() {
        let group = std::fs::read_to_string("/proc/self/cgroup")?;
        let delegated = format!("user@{}.service/", nix::unistd::geteuid());
        if !group.contains(&delegated) {
            // SSH sessions are outside the user's delegated subtree. Re-enter via systemd
            // before acquiring a lease; never run the user's command uncontained.
            let mut scope = Command::new("systemd-run");
            scope.args(["--user", "--scope", "--quiet", "--", "env", "NOMOS_RUN_SCOPED=1"])
                .arg(std::env::current_exe()?).arg("run")
                .args(["--cpu", &args.cpu.to_string(), "--mem", &args.memory, "--ttl", &args.ttl.to_string(), "--network", args.network_mode.as_str(), "--priority", &args.priority.to_string()]);
            if let Some(value) = &args.scratch { scope.args(["--scratch", value]); }
            if let Some(value) = &args.worker_id { scope.args(["--worker-id", value]); }
            if let Some(value) = &args.socket_path { scope.arg("--socket").arg(value); }
            for value in &args.devices { scope.args(["--device", value]); }
            for value in &args.depends_on { scope.args(["--depends-on", value]); }
            return Ok(scope.arg("--").args(&args.command).status().await?);
        }
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
        network_mode: args.network_mode,
        network_bandwidth_mbps: None,
        estimated_seconds: None,
        deadline: None,
        depends_on: args.depends_on,
        priority: Priority::from_u32(args.priority),
        ttl_seconds: args.ttl,
    };

    info!(
        "Worker '{}' requesting lease for command {:?} (CPU: {:.1}, RAM: {}, Network: {})...",
        worker_name, args.command, args.cpu, args.memory, args.network_mode
    );
    let lease = client.acquire_lease(lease_req).await?;
    info!("Lease {} GRANTED to worker '{}'! Starting child process...", lease.id, worker_name);

    let setup = (|| -> anyhow::Result<_> {
        let path = std::path::PathBuf::from(lease.cgroup_path.as_ref().ok_or_else(|| anyhow::anyhow!("Arbiter returned an unenforced lease"))?);
        nomos_sys::cgroup::require_cgroup2(&path)?;
        let procs = std::fs::OpenOptions::new().write(true).open(path.join("cgroup.procs"))?;
        Ok((path, procs))
    })();
    let (path, procs) = match setup {
        Ok(prepared) => prepared,
        Err(error) => {
            let cleanup = tokio::time::timeout(Duration::from_secs(5), client.call(crate::server::ArbiterRequest::FinishLease { lease_id: lease.id.clone(), success: false })).await;
            match cleanup {
                Ok(Ok(crate::server::ArbiterResponse::ReleasedAck { .. })) => return Err(error),
                other => anyhow::bail!("Runner preparation failed: {error}; cleanup unacknowledged: {other:?}"),
            }
        }
    };
    // Open before fork; write PID 0 in pre_exec before any workload instruction runs.
    let mut command = Command::new(&args.command[0]);
    command.args(&args.command[1..]).kill_on_drop(true);
    #[cfg(unix)]
    unsafe {
        use std::os::fd::AsRawFd;
        command.pre_exec(move || {
            let result = nix::libc::write(procs.as_raw_fd(), b"0".as_ptr().cast(), 1);
            if result != 1 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        });
    }
    let spawned = command.spawn();
    let outcome: Result<ExitStatus, anyhow::Error> = match spawned {
        Err(err) => Err(err.into()),
        Ok(mut child) => {
            let lease_id = lease.id.clone();
            let hb_sock = sock.clone();
            let heartbeat = async move {
                let mut heartbeat_client = NomosClient::connect(hb_sock).await?;
                loop {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    let alive = tokio::time::timeout(Duration::from_secs(3), heartbeat_client.heartbeat(&lease_id)).await??;
                    if !alive { anyhow::bail!("Lease revoked by arbiter"); }
                }
                #[allow(unreachable_code)] Ok::<(), anyhow::Error>(())
            };
            let result = tokio::select! {
                status = child.wait() => status.map_err(Into::into),
                hb = heartbeat => Err(hb.err().unwrap_or_else(|| anyhow::anyhow!("Heartbeat stopped"))),
                _ = tokio::time::sleep(Duration::from_secs(args.ttl)) => Err(anyhow::anyhow!("Workload TTL exceeded")),
                _ = shutdown_signal() => Err(anyhow::anyhow!("Runner interrupted")),
            };
            if result.is_err() {
                // Also works if the arbiter has disappeared. Descendants are in this cgroup.
                let kill_result = std::fs::write(path.join("cgroup.kill"), "1");
                let _ = child.kill().await;
                if let Err(err) = kill_result { tracing::error!("Failed to terminate workload cgroup: {}", err); }
            }
            result
        }
    };
    let success = outcome.as_ref().map(|status| status.success()).unwrap_or(false);
    let response = tokio::time::timeout(Duration::from_secs(5), client.call(crate::server::ArbiterRequest::FinishLease { lease_id: lease.id.clone(), success })).await;
    match response {
        Ok(Ok(crate::server::ArbiterResponse::ReleasedAck { .. })) => {},
        other => {
            let _ = std::fs::write(path.join("cgroup.kill"), "1");
            anyhow::bail!("Lease cleanup was not acknowledged: {:?}", other);
        }
    }
    outcome
}

async fn shutdown_signal() {
    #[cfg(unix)] {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))] { let _ = tokio::signal::ctrl_c().await; }
}
