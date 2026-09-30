mod actor;
mod cli;
mod client;
mod ipc;
mod mcp;
mod runner;
mod server;
mod tui;
mod web;

use clap::Parser;
use cli::{Cli, Commands};
use client::NomosClient;
use ipc::{default_socket_path, IpcServer};
use nomos_core::budget::{format_bytes, BudgetConfig, NomosPool};
use nomos_sys::probe::probe_host;
use std::fs;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Probe => {
            let hw = probe_host();
            println!("========================================================");
            println!(" NOMOS HARDWARE PROBE - Host Telemetry");
            println!("========================================================");
            println!(" Physical CPU Cores:      {}", hw.resources.total_cores);
            println!(" Physical RAM:            {}", format_bytes(hw.resources.total_memory_bytes));
            println!(" Physical Swap:           {}", format_bytes(hw.resources.total_swap_bytes));
            println!(" Total Storage Space:     {}", format_bytes(hw.resources.total_storage_bytes));
            println!("--------------------------------------------------------");
            println!(" GPU Accelerator:         {}", if hw.has_gpu { "DETECTED" } else { "None" });
            for d in &hw.gpu_details {
                println!("  - {}", d);
            }
            println!(" NPU Accelerator:         {}", if hw.has_npu { "DETECTED" } else { "None" });
            for d in &hw.npu_details {
                println!("  - {}", d);
            }
            println!("========================================================");
        }

        Commands::Daemon(args) => {
            tracing_subscriber::registry()
                .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
                .with(tracing_subscriber::fmt::layer())
                .init();

            info!("Starting Nomos Resource Arbiter Daemon...");

            let config: BudgetConfig = if let Some(ref path) = args.config {
                let content = fs::read_to_string(path)?;
                toml::from_str(&content)?
            } else {
                BudgetConfig::default()
            };

            let hw = probe_host();
            let pool = NomosPool::from_config(&config, &hw.resources)?;
            info!(
                "Configured Nomos Budget Pool: {:.1} Cores | {} RAM | {} Storage",
                pool.total_cores,
                format_bytes(pool.total_memory_bytes),
                format_bytes(pool.total_storage_bytes)
            );

            let audit_path = dirs_next().join("audit.jsonl").to_str().map(|s| s.to_string());
            let (handle, _actor_join) = actor::ArbiterActor::spawn(pool, audit_path);

            // Start IPC Unix Socket server
            let sock_path = args.socket.unwrap_or_else(default_socket_path);
            let ipc_server = IpcServer::new(sock_path, handle.clone());
            tokio::spawn(async move {
                if let Err(e) = ipc_server.run().await {
                    error!("IPC Server terminated with error: {}", e);
                }
            });

            // Start Web Server
            let web_port = args.port;
            let web_handle = handle.clone();
            tokio::spawn(async move {
                if let Err(e) = web::start_web_server(web_port, web_handle).await {
                    error!("Web server error: {}", e);
                }
            });

            info!("Nomos Arbiter daemon running. Press Ctrl+C to terminate.");
            tokio::signal::ctrl_c().await?;
            info!("Received shutdown signal, terminating Nomos daemon.");
        }

        Commands::Status(args) => {
            let sock = args.socket.unwrap_or_else(default_socket_path);
            let mut client = NomosClient::connect(&sock).await
                .map_err(|e| anyhow::anyhow!("Cannot connect to Nomos at {:?}: {}. Is 'nomos daemon' running?", sock, e))?;

            let status = client.get_status().await?;

            if args.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                println!("===============================================================================");
                println!(" NOMOS RESOURCE ARBITER STATUS");
                println!("===============================================================================");
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
                println!(
                    " CPU ALLOCATION:    {:.1} / {:.1} Cores ({:.1}% of Nomos Pool)",
                    alloc_cpu, status.pool.total_cores, cpu_pct
                );
                println!(
                    " RAM ALLOCATION:    {} / {} ({:.1}% of Nomos Pool)",
                    format_bytes(status.allocated_memory_bytes),
                    format_bytes(status.pool.total_memory_bytes),
                    mem_pct
                );
                println!(
                    " ACTIVE LEASES:     {} running",
                    status.active_leases.len()
                );
                println!(
                    " QUEUED LEASES:     {} waiting in line",
                    status.queued_leases.len()
                );
                println!("-------------------------------------------------------------------------------");
                if status.active_leases.is_empty() {
                    println!(" No active leases running.");
                } else {
                    println!(" ACTIVE LEASES:");
                    println!(" {:<12} {:<24} {:<8} {:<12} {:<10} {:<10}", "LEASE ID", "WORKER", "VCPU", "RAM", "PRIORITY", "STATE");
                    for l in &status.active_leases {
                        println!(
                            " {:<12} {:<24} {:<8.1} {:<12} {:<10?} {:<10?}",
                            l.id,
                            l.request.worker_id,
                            l.request.req_cpu,
                            format_bytes(l.request.req_memory_bytes),
                            l.request.priority,
                            l.state
                        );
                    }
                }
                if !status.queued_leases.is_empty() {
                    println!("-------------------------------------------------------------------------------");
                    println!(" QUEUED LEASES (Waiting for resources):");
                    for (i, q) in status.queued_leases.iter().enumerate() {
                        println!(" #{}: {} (Needs {:.1} vCPU, {})", i + 1, q.request.worker_id, q.request.req_cpu, format_bytes(q.request.req_memory_bytes));
                    }
                }
                println!("===============================================================================");
            }
        }

        Commands::Top(args) => {
            tui::run_tui(args.socket).await?;
        }

        Commands::Run(args) => {
            let run_args = runner::RunArgs {
                cpu: args.cpu,
                memory: args.mem,
                scratch: args.scratch,
                devices: args.devices,
                priority: args.priority,
                ttl: args.ttl,
                worker_id: args.worker_id,
                depends_on: args.depends_on,
                socket_path: args.socket,
                command: args.command,
                network_mode: args.network,
            };
            let status = runner::execute_runner(run_args).await?;
            if !status.success() {
                std::process::exit(status.code().unwrap_or(1));
            }
        }

        Commands::Accounting(args) => {
            let sock = args.socket.unwrap_or_else(default_socket_path);
            let mut client = NomosClient::connect(&sock).await
                .map_err(|e| anyhow::anyhow!("Cannot connect to Nomos at {:?}: {}. Is 'nomos daemon' running?", sock, e))?;

            let (summary, recent) = client.get_accounting(args.limit).await?;
            println!("===============================================================================");
            println!(" NOMOS RESOURCE ACCOUNTING LEDGER");
            println!("===============================================================================");
            println!(" Total Jobs Tracked:       {}", summary.total_records);
            println!(" Cumulative CPU Core-Hours: {:.4} hrs", summary.total_cpu_hours);
            println!(" Cumulative GPU Hours:      {:.4} hrs", summary.total_gpu_hours);
            println!(" Peak Memory Recorded:      {}", format_bytes(summary.peak_memory_seen_bytes));
            println!("-------------------------------------------------------------------------------");
            println!(" RECENT COMPLETED JOBS (Last {}):", recent.len());
            for r in recent {
                println!(
                    " - [{}] Worker: {:<16} Duration: {:.1}s  CPU-sec: {:.1}  Peak RAM: {}",
                    r.lease_id, r.worker_id, r.duration_seconds, r.cpu_core_seconds, format_bytes(r.peak_memory_bytes)
                );
            }
            println!("===============================================================================");
        }

        Commands::Update => {
            println!("===============================================================================");
            println!(" NOMOS SELF-UPDATE");
            println!("===============================================================================");
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let candidates = vec![
                std::path::PathBuf::from("install.sh"),
                std::path::PathBuf::from(&home).join("nomos/install.sh"),
                std::path::PathBuf::from(&home).join("WorkingSpace/ComposeLab/nomos/install.sh"),
            ];

            let installer = candidates.into_iter().find(|p| p.exists());
            if let Some(script) = installer {
                println!("Executing update script: {:?}", script);
                let status = std::process::Command::new("bash")
                    .arg(&script)
                    .status()?;
                if !status.success() {
                    std::process::exit(status.code().unwrap_or(1));
                }
            } else {
                eprintln!("[ERROR] Could not locate 'install.sh'. Please run from the Nomos repository directory.");
                std::process::exit(1);
            }
        }

        Commands::Mcp(args) => {
            // Route tracing exclusively to stderr so stdout is reserved for JSON-RPC 2.0 messages
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
                .init();

            mcp::run_mcp_server(args.socket).await?;
        }
    }

    Ok(())
}

fn dirs_next() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".nomos")
}
