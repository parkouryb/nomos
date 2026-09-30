use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct CgroupController {
    base_path: PathBuf,
    is_available: bool,
}

impl Default for CgroupController {
    fn default() -> Self {
        Self::new()
    }
}

impl CgroupController {
    pub fn new() -> Self {
        let candidate = PathBuf::from("/sys/fs/cgroup/nomos");
        let is_available = if Path::new("/sys/fs/cgroup").exists() {
            // Check if we can write to /sys/fs/cgroup
            match fs::create_dir_all(&candidate) {
                Ok(_) => true,
                Err(e) => {
                    warn!("Cgroups v2 not writable without root ({}), using user-space emulation", e);
                    false
                }
            }
        } else {
            false
        };

        Self {
            base_path: candidate,
            is_available,
        }
    }

    pub fn is_available(&self) -> bool {
        self.is_available
    }

    pub fn create_lease_slice(
        &self,
        lease_id: &str,
        vcpus: f64,
        memory_bytes: u64,
        max_iops: Option<u64>,
    ) -> Result<PathBuf, std::io::Error> {
        let slice = self.base_path.join(lease_id);

        if !self.is_available {
            info!("[Mock Cgroup] Created virtual slice for {} (CPU: {:.1}, RAM: {} B)", lease_id, vcpus, memory_bytes);
            return Ok(slice);
        }

        fs::create_dir_all(&slice)?;

        // 1. Configure CPU limits (cpu.max: $QUOTA $PERIOD)
        // Default period is 100,000 microseconds (100ms)
        let period = 100_000u64;
        let quota = (vcpus * period as f64) as u64;
        let cpu_max_str = format!("{} {}", quota, period);
        let _ = fs::write(slice.join("cpu.max"), cpu_max_str);

        // 2. Configure Memory limits (memory.max, memory.high)
        let mem_str = memory_bytes.to_string();
        let _ = fs::write(slice.join("memory.max"), &mem_str);
        // memory.high set to 90% of max for proactive page reclamation
        let high_str = ((memory_bytes as f64 * 0.9) as u64).to_string();
        let _ = fs::write(slice.join("memory.high"), &high_str);

        // 3. Configure IO limits if available
        if let Some(iops) = max_iops {
            let io_str = format!("default riops={} wiops={}", iops, iops);
            let _ = fs::write(slice.join("io.max"), io_str);
        }

        Ok(slice)
    }

    pub fn attach_pid(&self, lease_id: &str, pid: u32) -> Result<(), std::io::Error> {
        if !self.is_available {
            info!("[Mock Cgroup] Attached PID {} to virtual slice {}", pid, lease_id);
            return Ok(());
        }

        let procs_file = self.base_path.join(lease_id).join("cgroup.procs");
        fs::write(procs_file, pid.to_string())
    }

    pub fn destroy_lease_slice(&self, lease_id: &str) -> Result<(), std::io::Error> {
        let slice = self.base_path.join(lease_id);
        if slice.exists() {
            let _ = fs::remove_dir_all(&slice);
        }
        Ok(())
    }

    pub fn reconcile_and_clean_orphans(&self) -> usize {
        let mut cleaned = 0;
        if !self.is_available {
            return 0;
        }

        if let Ok(entries) = fs::read_dir(&self.base_path) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let path = entry.path();
                    // If no active procs inside, remove dead cgroup
                    let procs_path = path.join("cgroup.procs");
                    if let Ok(content) = fs::read_to_string(&procs_path) {
                        if content.trim().is_empty() {
                            let _ = fs::remove_dir(&path);
                            cleaned += 1;
                        }
                    }
                }
            }
        }
        cleaned
    }
}
