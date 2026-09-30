use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BudgetError {
    #[error("Invalid percentage: {0}")]
    InvalidPercentage(String),
    #[error("Invalid byte format: {0}")]
    InvalidByteFormat(String),
    #[error("Calculated budget exceeds physical host capacity")]
    CapacityExceeded,
    #[error("Host safety reserve violated: requires {required} B, but host has {available} B")]
    HostSafetyViolation { required: u64, available: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhysicalHostResources {
    pub total_cores: usize,
    pub total_memory_bytes: u64,
    pub total_swap_bytes: u64,
    pub total_storage_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    #[serde(default = "default_mode")]
    pub mode: String, // "percentage" or "absolute"
    #[serde(default = "default_cpu_limit")]
    pub cpu_limit: String, // e.g. "50%" or "7.0"
    #[serde(default = "default_memory_limit")]
    pub memory_limit: String, // e.g. "30%" or "19.2GB"
    #[serde(default = "default_host_reserve_memory")]
    pub host_reserve_memory: String, // e.g. "8GB"
    #[serde(default = "default_storage_limit")]
    pub storage_limit: String, // e.g. "200GB"
    #[serde(default = "default_max_active_leases")]
    pub max_active_leases: usize,
}

fn default_mode() -> String {
    "percentage".to_string()
}
fn default_cpu_limit() -> String {
    "50%".to_string()
}
fn default_memory_limit() -> String {
    "50%".to_string()
}
fn default_host_reserve_memory() -> String {
    "8GB".to_string()
}
fn default_storage_limit() -> String {
    "200GB".to_string()
}
fn default_max_active_leases() -> usize {
    100
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            cpu_limit: default_cpu_limit(),
            memory_limit: default_memory_limit(),
            host_reserve_memory: default_host_reserve_memory(),
            storage_limit: default_storage_limit(),
            max_active_leases: default_max_active_leases(),
        }
    }
}

/// The effective pool of resources managed by Nomos.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NomosPool {
    pub total_cores: f64,
    pub total_memory_bytes: u64,
    pub host_reserve_memory_bytes: u64,
    pub total_storage_bytes: u64,
    pub max_active_leases: usize,
}

impl NomosPool {
    pub fn from_config(
        config: &BudgetConfig,
        host: &PhysicalHostResources,
    ) -> Result<Self, BudgetError> {
        let host_reserve_bytes = parse_bytes_string(&config.host_reserve_memory)?;
        if host_reserve_bytes >= host.total_memory_bytes {
            return Err(BudgetError::HostSafetyViolation {
                required: host_reserve_bytes,
                available: host.total_memory_bytes,
            });
        }

        let total_cores = if config.cpu_limit.ends_with('%') {
            let pct = parse_percent_string(&config.cpu_limit)?;
            (host.total_cores as f64) * (pct / 100.0)
        } else {
            config
                .cpu_limit
                .parse::<f64>()
                .map_err(|_| BudgetError::InvalidPercentage(config.cpu_limit.clone()))?
        };

        let total_memory_bytes = if config.memory_limit.ends_with('%') {
            let pct = parse_percent_string(&config.memory_limit)?;
            let raw = (host.total_memory_bytes as f64) * (pct / 100.0);
            raw as u64
        } else {
            parse_bytes_string(&config.memory_limit)?
        };

        // Safety check: Total memory allocated to Nomos + Reserve cannot exceed total host memory
        if total_memory_bytes + host_reserve_bytes > host.total_memory_bytes {
            return Err(BudgetError::CapacityExceeded);
        }

        let total_storage_bytes = parse_bytes_string(&config.storage_limit)?;

        Ok(Self {
            total_cores,
            total_memory_bytes,
            host_reserve_memory_bytes: host_reserve_bytes,
            total_storage_bytes,
            max_active_leases: config.max_active_leases,
        })
    }
}

pub fn parse_percent_string(s: &str) -> Result<f64, BudgetError> {
    let clean = s.trim().trim_end_matches('%').trim();
    clean
        .parse::<f64>()
        .map_err(|_| BudgetError::InvalidPercentage(s.to_string()))
}

pub fn parse_bytes_string(s: &str) -> Result<u64, BudgetError> {
    let raw = s.trim().to_uppercase();
    if raw.ends_with("TIB") || raw.ends_with("TB") {
        let num: f64 = raw
            .trim_end_matches("TIB")
            .trim_end_matches("TB")
            .trim()
            .parse()
            .map_err(|_| BudgetError::InvalidByteFormat(s.to_string()))?;
        Ok((num * 1024.0 * 1024.0 * 1024.0 * 1024.0) as u64)
    } else if raw.ends_with("GIB") || raw.ends_with("GB") {
        let num: f64 = raw
            .trim_end_matches("GIB")
            .trim_end_matches("GB")
            .trim()
            .parse()
            .map_err(|_| BudgetError::InvalidByteFormat(s.to_string()))?;
        Ok((num * 1024.0 * 1024.0 * 1024.0) as u64)
    } else if raw.ends_with("MIB") || raw.ends_with("MB") {
        let num: f64 = raw
            .trim_end_matches("MIB")
            .trim_end_matches("MB")
            .trim()
            .parse()
            .map_err(|_| BudgetError::InvalidByteFormat(s.to_string()))?;
        Ok((num * 1024.0 * 1024.0) as u64)
    } else if raw.ends_with("KIB") || raw.ends_with("KB") {
        let num: f64 = raw
            .trim_end_matches("KIB")
            .trim_end_matches("KB")
            .trim()
            .parse()
            .map_err(|_| BudgetError::InvalidByteFormat(s.to_string()))?;
        Ok((num * 1024.0) as u64)
    } else if raw.ends_with('B') {
        let num: u64 = raw
            .trim_end_matches('B')
            .trim()
            .parse()
            .map_err(|_| BudgetError::InvalidByteFormat(s.to_string()))?;
        Ok(num)
    } else {
        raw.parse::<u64>()
            .map_err(|_| BudgetError::InvalidByteFormat(s.to_string()))
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;

    if bytes >= GIB {
        format!("{:.2} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bytes() {
        assert_eq!(parse_bytes_string("64GB").unwrap(), 64 * 1024 * 1024 * 1024);
        assert_eq!(parse_bytes_string("512MB").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_bytes_string("1024").unwrap(), 1024);
    }

    #[test]
    fn test_budget_pool_calculation() {
        let host = PhysicalHostResources {
            total_cores: 14,
            total_memory_bytes: 64 * 1024 * 1024 * 1024,
            total_swap_bytes: 8 * 1024 * 1024 * 1024,
            total_storage_bytes: 1000 * 1024 * 1024 * 1024,
        };

        let cfg = BudgetConfig {
            mode: "percentage".into(),
            cpu_limit: "50%".into(),
            memory_limit: "30%".into(),
            host_reserve_memory: "8GB".into(),
            storage_limit: "200GB".into(),
            max_active_leases: 100,
        };

        let pool = NomosPool::from_config(&cfg, &host).unwrap();
        assert_eq!(pool.total_cores, 7.0);
        assert_eq!(pool.total_memory_bytes, (64.0 * 0.3 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(pool.total_storage_bytes, 200 * 1024 * 1024 * 1024);
    }
}
