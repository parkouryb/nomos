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
    retention_days: u32,
    in_memory: Vec<AuditRecord>,
}

impl AuditLedger {
    pub fn new(log_file_path: Option<String>) -> Self {
        Self::with_retention(log_file_path, 30)
    }

    pub fn with_retention(log_file_path: Option<String>, retention_days: u32) -> Self {
        let mut ledger = Self {
            log_file_path,
            retention_days: if retention_days == 0 { 30 } else { retention_days },
            in_memory: Vec::new(),
        };
        ledger.load_and_prune();
        ledger
    }

    fn load_and_prune(&mut self) {
        if let Some(ref path) = self.log_file_path {
            if Path::new(path).exists() {
                if let Ok(file) = std::fs::File::open(path) {
                    use std::io::BufRead;
                    let reader = std::io::BufReader::new(file);
                    let cutoff = Utc::now() - chrono::Duration::days(self.retention_days as i64);
                    let mut loaded = Vec::new();
                    let mut pruned_any = false;

                    for line in reader.lines().flatten() {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Ok(rec) = serde_json::from_str::<AuditRecord>(trimmed) {
                            if rec.finished_at >= cutoff {
                                loaded.push(rec);
                            } else {
                                pruned_any = true;
                            }
                        }
                    }

                    self.in_memory = loaded;

                    // If any records expired, rewrite file cleanly
                    if pruned_any {
                        let _ = self.rewrite_file();
                    }
                }
            }
        }
    }

    fn rewrite_file(&self) -> std::io::Result<()> {
        if let Some(ref path) = self.log_file_path {
            let tmp_path = format!("{}.tmp", path);
            {
                let mut f = std::fs::File::create(&tmp_path)?;
                for r in &self.in_memory {
                    if let Ok(json) = serde_json::to_string(r) {
                        writeln!(f, "{}", json)?;
                    }
                }
                f.flush()?;
            }
            std::fs::rename(tmp_path, path)?;
        }
        Ok(())
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

        // Prune periodically every 100 insertions
        if self.in_memory.len() % 100 == 0 {
            self.prune();
        }
    }

    pub fn prune(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::days(self.retention_days as i64);
        let initial_len = self.in_memory.len();
        self.in_memory.retain(|r| r.finished_at >= cutoff);
        if self.in_memory.len() != initial_len {
            let _ = self.rewrite_file();
        }
    }

    pub fn summary(&self) -> AccountingSummary {
        self.summary_window(None)
    }

    pub fn summary_window(&self, days: Option<u32>) -> AccountingSummary {
        let cutoff = days.map(|d| Utc::now() - chrono::Duration::days(d as i64));
        let mut sum = AccountingSummary::default();
        for r in &self.in_memory {
            if let Some(c) = cutoff {
                if r.finished_at < c {
                    continue;
                }
            }
            sum.total_records += 1;
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
        self.recent_records_window(limit, None)
    }

    pub fn recent_records_window(&self, limit: usize, _days: Option<u32>) -> &[AuditRecord] {
        let len = self.in_memory.len();
        if len > limit {
            &self.in_memory[len - limit..]
        } else {
            &self.in_memory[..]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audit_ledger_summary_and_window() {
        let mut ledger = AuditLedger::with_retention(None, 30);
        let now = Utc::now();

        ledger.record(AuditRecord {
            lease_id: "ls-1".into(),
            worker_id: "worker-1".into(),
            tenant: "test".into(),
            started_at: now - chrono::Duration::seconds(100),
            finished_at: now,
            duration_seconds: 100.0,
            cpu_core_seconds: 200.0,
            peak_memory_bytes: 1024 * 1024 * 512,
            gpu_seconds: 0.0,
            net_egress_bytes: 0,
            final_state: "Completed".into(),
        });

        let summary = ledger.summary();
        assert_eq!(summary.total_records, 1);
        assert_eq!(summary.peak_memory_seen_bytes, 1024 * 1024 * 512);

        let window_7d = ledger.summary_window(Some(7));
        assert_eq!(window_7d.total_records, 1);
    }

    #[test]
    fn test_audit_ledger_retention_pruning() {
        let mut ledger = AuditLedger::with_retention(None, 30);
        let now = Utc::now();

        // Old record: 45 days ago
        ledger.in_memory.push(AuditRecord {
            lease_id: "ls-old".into(),
            worker_id: "worker-old".into(),
            tenant: "test".into(),
            started_at: now - chrono::Duration::days(46),
            finished_at: now - chrono::Duration::days(45),
            duration_seconds: 10.0,
            cpu_core_seconds: 20.0,
            peak_memory_bytes: 1024,
            gpu_seconds: 0.0,
            net_egress_bytes: 0,
            final_state: "Completed".into(),
        });

        // Recent record: 5 days ago
        ledger.in_memory.push(AuditRecord {
            lease_id: "ls-recent".into(),
            worker_id: "worker-recent".into(),
            tenant: "test".into(),
            started_at: now - chrono::Duration::days(5),
            finished_at: now - chrono::Duration::days(5),
            duration_seconds: 10.0,
            cpu_core_seconds: 20.0,
            peak_memory_bytes: 2048,
            gpu_seconds: 0.0,
            net_egress_bytes: 0,
            final_state: "Completed".into(),
        });

        assert_eq!(ledger.in_memory.len(), 2);
        ledger.prune();
        assert_eq!(ledger.in_memory.len(), 1);
        assert_eq!(ledger.in_memory[0].lease_id, "ls-recent");
    }
}
