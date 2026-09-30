use nomos_core::lease::NetworkMode;
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
                Ok(_) => {
                    // Pre-create network mode directories and enable subtree control
                    let _ = fs::write(candidate.join("cgroup.subtree_control"), "+cpu +memory +io");
                    for mode in ["isolated", "none", "host"] {
                        let mdir = candidate.join(mode);
                        let _ = fs::create_dir_all(&mdir);
                        let _ = fs::write(mdir.join("cgroup.subtree_control"), "+cpu +memory +io");
                    }
                    true
                }
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

    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    pub fn slice_path(&self, lease_id: &str, network_mode: NetworkMode) -> PathBuf {
        self.base_path.join(network_mode.as_str()).join(lease_id)
    }

    pub fn create_lease_slice(
        &self,
        lease_id: &str,
        vcpus: f64,
        memory_bytes: u64,
        max_iops: Option<u64>,
        network_mode: NetworkMode,
    ) -> Result<PathBuf, std::io::Error> {
        let slice = self.slice_path(lease_id, network_mode);

        if !self.is_available {
            info!(
                "[Mock Cgroup] Created virtual slice for {} (mode: {}, CPU: {:.1}, RAM: {} B)",
                lease_id, network_mode, vcpus, memory_bytes
            );
            return Ok(slice);
        }

        let mode_dir = self.base_path.join(network_mode.as_str());
        fs::create_dir_all(&mode_dir)?;
        let _ = fs::write(mode_dir.join("cgroup.subtree_control"), "+cpu +memory +io");

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

    pub fn attach_pid_with_mode(&self, lease_id: &str, network_mode: NetworkMode, pid: u32) -> Result<(), std::io::Error> {
        if !self.is_available {
            info!("[Mock Cgroup] Attached PID {} to virtual slice {} (mode: {})", pid, lease_id, network_mode);
            return Ok(());
        }

        let slice = self.slice_path(lease_id, network_mode);
        let procs_file = slice.join("cgroup.procs");
        if procs_file.exists() {
            fs::write(procs_file, pid.to_string())
        } else {
            // Fallback to checking other paths
            self.attach_pid(lease_id, pid)
        }
    }

    pub fn attach_pid(&self, lease_id: &str, pid: u32) -> Result<(), std::io::Error> {
        if !self.is_available {
            info!("[Mock Cgroup] Attached PID {} to virtual slice {}", pid, lease_id);
            return Ok(());
        }

        // Search in mode directories first
        for mode in [NetworkMode::Isolated, NetworkMode::None, NetworkMode::Host] {
            let procs_file = self.slice_path(lease_id, mode).join("cgroup.procs");
            if procs_file.exists() {
                return fs::write(procs_file, pid.to_string());
            }
        }

        // Fallback to base flat directory
        let flat_procs = self.base_path.join(lease_id).join("cgroup.procs");
        if flat_procs.exists() {
            return fs::write(flat_procs, pid.to_string());
        }

        // If not created yet, default to isolated slice
        let def_slice = self.slice_path(lease_id, NetworkMode::Isolated);
        let _ = fs::create_dir_all(&def_slice);
        fs::write(def_slice.join("cgroup.procs"), pid.to_string())
    }

    pub fn destroy_lease_slice(&self, lease_id: &str) -> Result<(), std::io::Error> {
        for mode in [NetworkMode::Isolated, NetworkMode::None, NetworkMode::Host] {
            let slice = self.slice_path(lease_id, mode);
            if slice.exists() {
                let _ = fs::remove_dir_all(&slice);
            }
        }
        let flat = self.base_path.join(lease_id);
        if flat.exists() {
            let _ = fs::remove_dir_all(&flat);
        }
        Ok(())
    }

    pub fn reconcile_and_clean_orphans(&self) -> usize {
        let mut cleaned = 0;
        if !self.is_available {
            return 0;
        }

        // Clean sub-directories in mode folders
        for mode in ["isolated", "none", "host"] {
            let mode_dir = self.base_path.join(mode);
            if let Ok(entries) = fs::read_dir(&mode_dir) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        let path = entry.path();
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
        }

        // Clean flat legacy entries
        if let Ok(entries) = fs::read_dir(&self.base_path) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name == "isolated" || name == "none" || name == "host" {
                        continue;
                    }
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
