use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "nomos", version, about = "Nomos Single-Host Resource Arbiter")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start the Nomos arbiter background daemon
    Daemon(DaemonArgs),
    /// Probe and display physical host hardware, accelerators and devices
    Probe,
    /// Query and display current resource status from running daemon
    Status(StatusArgs),
    /// Launch interactive TUI terminal monitor
    Top(TopArgs),
    /// Execute a command wrapped inside a Nomos resource lease
    Run(RunCliArgs),
    /// Query resource accounting and audit logs
    Accounting(AccountingArgs),
    /// Recompile, update Nomos binary, configuration, and restart background daemon
    Update,
    /// Run Model Context Protocol (MCP) server over stdio for LLMs (Claude Desktop, Cursor)
    Mcp(McpArgs),
}

#[derive(Args, Debug)]
pub struct DaemonArgs {
    /// Path to TOML configuration file
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    /// Web dashboard HTTP port (default: 9100)
    #[arg(short, long, default_value = "9100")]
    pub port: u16,
    /// Custom Unix socket path
    #[arg(short, long)]
    pub socket: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Output raw JSON instead of formatted table
    #[arg(long)]
    pub json: bool,
    /// Custom Unix socket path
    #[arg(short, long)]
    pub socket: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct TopArgs {
    /// Custom Unix socket path
    #[arg(short, long)]
    pub socket: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct RunCliArgs {
    /// Required CPU cores (e.g. 1.0, 2.5)
    #[arg(long, default_value = "1.0")]
    pub cpu: f64,
    /// Required RAM (e.g. 512MB, 4GB)
    #[arg(long, default_value = "1GB")]
    pub mem: String,
    /// Required temporary scratch space (e.g. 2GB)
    #[arg(long)]
    pub scratch: Option<String>,
    /// Required hardware devices (e.g. gpu, npu)
    #[arg(long = "device")]
    pub devices: Vec<String>,
    /// Network egress isolation mode (isolated: LAN & loopback only, none: air-gapped, host: full host network)
    #[arg(long = "network", default_value = "isolated")]
    pub network: nomos_core::lease::NetworkMode,
    /// Priority level (10 = Low, 50 = Normal, 75 = High, 100 = Critical)
    #[arg(long, default_value = "50")]
    pub priority: u32,
    /// Time-to-live in seconds before auto-reclaiming (default: 60)
    #[arg(long, default_value = "60")]
    pub ttl: u64,
    /// Worker identifier for DAG tracking (defaults to cli-<cmd>)
    #[arg(long)]
    pub worker_id: Option<String>,
    /// Workers that must finish before this worker can run
    #[arg(long = "depends-on")]
    pub depends_on: Vec<String>,
    /// Custom Unix socket path
    #[arg(short, long)]
    pub socket: Option<PathBuf>,
    /// Command to execute
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct AccountingArgs {
    /// Time window in days to aggregate accounting metrics (default: 30)
    #[arg(long, default_value = "30")]
    pub days: u32,
    /// Limit number of recent records
    #[arg(long, default_value = "20")]
    pub limit: usize,
    /// Custom Unix socket path
    #[arg(short, long)]
    pub socket: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct McpArgs {
    /// Custom Unix socket path to Nomos arbiter daemon
    #[arg(short, long)]
    pub socket: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nomos_core::lease::NetworkMode;

    #[test]
    fn test_cli_mcp_command() {
        let args = Cli::try_parse_from(["nomos", "mcp"]).unwrap();
        if let Commands::Mcp(mcp_args) = args.command {
            assert!(mcp_args.socket.is_none());
        } else {
            panic!("Expected Commands::Mcp");
        }

        let args_sock = Cli::try_parse_from(["nomos", "mcp", "--socket", "/tmp/test.sock"]).unwrap();
        if let Commands::Mcp(mcp_args) = args_sock.command {
            assert_eq!(mcp_args.socket.unwrap(), PathBuf::from("/tmp/test.sock"));
        } else {
            panic!("Expected Commands::Mcp");
        }
    }

    #[test]
    fn test_cli_network_flag_default() {
        let args = Cli::try_parse_from(["nomos", "run", "--", "echo", "hello"]).unwrap();
        if let Commands::Run(run_args) = args.command {
            assert_eq!(run_args.network, NetworkMode::Isolated);
            assert_eq!(run_args.command, vec!["echo", "hello"]);
        } else {
            panic!("Expected Commands::Run");
        }
    }

    #[test]
    fn test_cli_network_flag_explicit() {
        let args_none = Cli::try_parse_from(["nomos", "run", "--network", "none", "--", "ls"]).unwrap();
        if let Commands::Run(run_args) = args_none.command {
            assert_eq!(run_args.network, NetworkMode::None);
        } else {
            panic!("Expected Commands::Run");
        }

        let args_host = Cli::try_parse_from(["nomos", "run", "--network", "host", "--", "ls"]).unwrap();
        if let Commands::Run(run_args) = args_host.command {
            assert_eq!(run_args.network, NetworkMode::Host);
        } else {
            panic!("Expected Commands::Run");
        }

        let args_isolated = Cli::try_parse_from(["nomos", "run", "--network", "isolated", "--", "ls"]).unwrap();
        if let Commands::Run(run_args) = args_isolated.command {
            assert_eq!(run_args.network, NetworkMode::Isolated);
        } else {
            panic!("Expected Commands::Run");
        }
    }

    #[test]
    fn test_cli_network_flag_invalid() {
        let res = Cli::try_parse_from(["nomos", "run", "--network", "invalid", "--", "ls"]);
        assert!(res.is_err());
    }
}

