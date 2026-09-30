use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type LeaseId = String;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Priority {
    Low = 10,
    Normal = 50,
    High = 75,
    Critical = 100,
}

impl Default for Priority {
    fn default() -> Self {
        Self::Normal
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NetworkMode {
    #[serde(rename = "isolated")]
    Isolated, // Localhost & LAN only
    #[serde(rename = "none")]
    None,     // Air-gapped
    #[serde(rename = "host")]
    Host,     // Full host networking
}

impl Default for NetworkMode {
    fn default() -> Self {
        Self::Isolated
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
