use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub lease_id: String,
    pub worker_id: String,
    pub tenant: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_seconds: f64,
    pub cpu_core_seconds: f64,
    pub peak_memory_bytes: u64,
    pub gpu_seconds: f64,
    pub net_egress_bytes: u64,
    pub final_state: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountingSummary {
    pub total_records: usize,
    pub total_cpu_hours: f64,
    pub total_gpu_hours: f64,
    pub total_duration_hours: f64,
    pub peak_memory_seen_bytes: u64,
}

pub struct AuditLedger {
    log_file_path: Option<String>,
    in_memory: Vec<AuditRecord>,
}

impl AuditLedger {
    pub fn new(log_file_path: Option<String>) -> Self {
        Self {
            log_file_path,
            in_memory: Vec::new(),
        }
    }

    pub fn record(&mut self, rec: AuditRecord) {
        if let Some(ref path) = self.log_file_path {
            if let Ok(json) = serde_json::to_string(&rec) {
                if let Some(parent) = Path::new(path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = writeln!(f, "{}", json);
                }
            }
        }
        self.in_memory.push(rec);
    }

    pub fn summary(&self) -> AccountingSummary {
        let mut sum = AccountingSummary::default();
        sum.total_records = self.in_memory.len();
        for r in &self.in_memory {
            sum.total_cpu_hours += r.cpu_core_seconds / 3600.0;
            sum.total_gpu_hours += r.gpu_seconds / 3600.0;
            sum.total_duration_hours += r.duration_seconds / 3600.0;
            if r.peak_memory_bytes > sum.peak_memory_seen_bytes {
                sum.peak_memory_seen_bytes = r.peak_memory_bytes;
            }
        }
        sum
    }

    pub fn recent_records(&self, limit: usize) -> &[AuditRecord] {
        let len = self.in_memory.len();
        if len > limit {
            &self.in_memory[len - limit..]
        } else {
            &self.in_memory[..]
        }
    }
}
