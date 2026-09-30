use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type LeaseId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub enum Priority {
    Low = 10,
    #[default]
    Normal = 50,
    High = 75,
    Critical = 100,
}


impl Priority {
    pub fn from_u32(val: u32) -> Self {
        if val >= 90 {
            Priority::Critical
        } else if val >= 70 {
            Priority::High
        } else if val >= 30 {
            Priority::Normal
        } else {
            Priority::Low
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceType {
    Gpu(String), // e.g. "arc" or "renderD128"
    Npu(String), // e.g. "accel0"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum NetworkMode {
    #[serde(rename = "isolated")]
    #[default]
    Isolated, // Localhost & LAN only
    #[serde(rename = "none")]
    None,     // Air-gapped
    #[serde(rename = "host")]
    Host,     // Full host networking
}


impl NetworkMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::None => "none",
            Self::Host => "host",
        }
    }
}

impl std::str::FromStr for NetworkMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "isolated" => Ok(NetworkMode::Isolated),
            "none" => Ok(NetworkMode::None),
            "host" => Ok(NetworkMode::Host),
            other => Err(format!(
                "Invalid network mode '{}'. Valid options are: isolated, none, host",
                other
            )),
        }
    }
}

impl std::fmt::Display for NetworkMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseState {
    Pending,
    Granted,
    Running,
    Preempted, // Paused by higher priority task via SIGSTOP
    Completed,
    Expired,
    Revoked,
    Hung,      // Heartbeat dead-man's switch triggered
}

impl LeaseState {
    pub fn is_active(&self) -> bool {
        matches!(self, LeaseState::Granted | LeaseState::Running | LeaseState::Preempted)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, LeaseState::Completed | LeaseState::Expired | LeaseState::Revoked | LeaseState::Hung)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRequest {
    pub worker_id: String,
    #[serde(default = "default_tenant")]
    pub tenant: String,
    pub req_cpu: f64,
    pub req_memory_bytes: u64,
    #[serde(default)]
    pub req_scratch_bytes: u64,
    #[serde(default)]
    pub devices: Vec<DeviceType>,
    #[serde(default)]
    pub network_mode: NetworkMode,
    #[serde(default)]
    pub network_bandwidth_mbps: Option<u32>,
    pub estimated_seconds: Option<f64>,
    pub deadline: Option<DateTime<Utc>>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default = "default_ttl")]
    pub ttl_seconds: u64,
}

fn default_tenant() -> String {
    "default".to_string()
}
fn default_ttl() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub id: String,
    pub request: LeaseRequest,
    pub state: LeaseState,
    pub created_at: DateTime<Utc>,
    pub granted_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub scratch_path: Option<String>,
    pub cgroup_path: Option<String>,
    pub actual_runtime_seconds: f64,
}

impl Lease {
    pub fn new(request: LeaseRequest) -> Self {
        let now = Utc::now();
        let id = format!("ls-{}", Uuid::new_v4().to_string().chars().take(8).collect::<String>());
        Self {
            id,
            request,
            state: LeaseState::Pending,
            created_at: now,
            granted_at: None,
            expires_at: None,
            last_heartbeat: None,
            scratch_path: None,
            cgroup_path: None,
            actual_runtime_seconds: 0.0,
        }
    }

    pub fn mark_granted(&mut self, ttl_seconds: u64, scratch_path: Option<String>, cgroup_path: Option<String>) {
        let now = Utc::now();
        self.state = LeaseState::Granted;
        self.granted_at = Some(now);
        self.expires_at = Some(now + chrono::Duration::seconds(ttl_seconds as i64));
        self.last_heartbeat = Some(now);
        self.scratch_path = scratch_path;
        self.cgroup_path = cgroup_path;
    }

    pub fn mark_running(&mut self) {
        if self.state == LeaseState::Granted || self.state == LeaseState::Preempted {
            self.state = LeaseState::Running;
            self.last_heartbeat = Some(Utc::now());
        }
    }

    pub fn record_heartbeat(&mut self) {
        self.last_heartbeat = Some(Utc::now());
    }

    pub fn mark_completed(&mut self) {
        self.state = LeaseState::Completed;
        if let Some(granted) = self.granted_at {
            self.actual_runtime_seconds = (Utc::now() - granted).num_milliseconds() as f64 / 1000.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_network_mode_parsing() {
        assert_eq!(NetworkMode::from_str("isolated").unwrap(), NetworkMode::Isolated);
        assert_eq!(NetworkMode::from_str("ISOLATED").unwrap(), NetworkMode::Isolated);
        assert_eq!(NetworkMode::from_str("none").unwrap(), NetworkMode::None);
        assert_eq!(NetworkMode::from_str("NONE").unwrap(), NetworkMode::None);
        assert_eq!(NetworkMode::from_str("host").unwrap(), NetworkMode::Host);
        assert_eq!(NetworkMode::from_str("HOST").unwrap(), NetworkMode::Host);

        assert!(NetworkMode::from_str("invalid").is_err());
        assert!(NetworkMode::from_str("wan").is_err());
    }

    #[test]
    fn test_network_mode_display() {
        assert_eq!(NetworkMode::Isolated.to_string(), "isolated");
        assert_eq!(NetworkMode::None.to_string(), "none");
        assert_eq!(NetworkMode::Host.to_string(), "host");
    }
}

