//! Kernel-backed resource enforcement. There is no emulated success path.
use nomos_core::lease::NetworkMode;
use std::{fs, io, path::{Path, PathBuf}};

#[derive(Debug, Clone)]
pub struct CgroupController { base_path: PathBuf }
fn invalid(message: impl Into<String>) -> io::Error { io::Error::new(io::ErrorKind::Other, message.into()) }

pub fn require_cgroup2(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("Invalid cgroup path"))?;
        let mut stat = std::mem::MaybeUninit::<nix::libc::statfs>::uninit();
        if unsafe { nix::libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 { return Err(io::Error::last_os_error()); }
        if unsafe { stat.assume_init() }.f_type as u64 != 0x63677270 { return Err(invalid("Resource enforcement requires a real cgroup v2 filesystem")); }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = path; Err(invalid("Kernel resource enforcement requires Linux cgroup v2")) }
}

impl Default for CgroupController { fn default() -> Self { Self::new() } }
impl CgroupController {
    pub fn new() -> Self {
        let base_path = std::env::var_os("NOMOS_CGROUP_ROOT").map(PathBuf::from).unwrap_or_else(|| {
            let current = fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
            let relative = current.lines().find_map(|line| line.strip_prefix("0::")).unwrap_or("");
            if relative.ends_with(".service") { Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')) }
            else { PathBuf::from("/sys/fs/cgroup/nomos") }
        });
        Self { base_path }
    }
    /// Called only by the daemon in its dedicated systemd Delegate=yes cgroup.
    pub fn initialize(&self) -> io::Result<()> {
        require_cgroup2(&self.base_path)?;
        let controllers = fs::read_to_string(self.base_path.join("cgroup.controllers"))?;
        for name in ["cpu", "memory"] { if !controllers.split_whitespace().any(|s| s == name) { return Err(invalid(format!("Missing delegated controller: {name}"))); } }
        let manager = self.base_path.join("manager");
        fs::create_dir_all(&manager)?;
        fs::write(manager.join("cgroup.procs"), std::process::id().to_string())?;
        fs::write(self.base_path.join("cgroup.subtree_control"), "+cpu +memory")?;
        let host = self.base_path.join("host");
        fs::create_dir_all(&host)?;
        fs::write(host.join("cgroup.subtree_control"), "+cpu +memory")?;
        Ok(())
    }
    pub fn is_available(&self) -> bool { require_cgroup2(&self.base_path).is_ok() && self.base_path.join("host/cgroup.subtree_control").exists() }
    pub fn base_path(&self) -> &Path { &self.base_path }
    pub fn slice_path(&self, id: &str, mode: NetworkMode) -> PathBuf { self.base_path.join(mode.as_str()).join(id) }
    fn validate_id(id: &str) -> io::Result<()> {
        if !id.starts_with("ls-") || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') { return Err(invalid("Invalid lease ID")); } Ok(())
    }
    pub fn create_lease_slice(&self, id: &str, cpu: f64, memory: u64, iops: Option<u64>, mode: NetworkMode) -> io::Result<PathBuf> {
        Self::validate_id(id)?; require_cgroup2(&self.base_path)?;
        if mode != NetworkMode::Host { return Err(invalid("Network isolation is not verified; explicitly request host networking")); }
        if iops.is_some() { return Err(invalid("IOPS enforcement requires a verified block-device policy")); }
        if !cpu.is_finite() || cpu <= 0.0 || memory < 4096 { return Err(invalid("CPU must be finite and positive; RAM must be at least 4096 bytes")); }
        let slice = self.slice_path(id, mode);
        fs::create_dir(&slice)?;
        let quota = (cpu * 100_000.0).ceil() as u64;
        let configured = (|| {
            fs::write(slice.join("cpu.max"), format!("{quota} 100000"))?;
            fs::write(slice.join("memory.max"), memory.to_string())?;
            fs::write(slice.join("memory.high"), (memory - memory / 10).to_string())?;
            // Prevent swap from silently extending the memory reservation.
            fs::write(slice.join("memory.swap.max"), "0")?;
            fs::write(slice.join("memory.oom.group"), "1")?;
            let cpu_read = fs::read_to_string(slice.join("cpu.max"))?;
            if cpu_read.split_whitespace().collect::<Vec<_>>() != [quota.to_string(), "100000".into()] { return Err(invalid("CPU limit readback mismatch")); }
            let mem_read: u64 = fs::read_to_string(slice.join("memory.max"))?.trim().parse().map_err(|_| invalid("Invalid memory limit readback"))?;
            // Linux rounds memory limits to pages. Never allow a rounded-up limit.
            if mem_read > memory || mem_read == 0 { return Err(invalid("Memory limit readback mismatch")); }
            if fs::read_to_string(slice.join("memory.swap.max"))?.trim() != "0" { return Err(invalid("Swap limit readback mismatch")); }
            Ok(())
        })();
        if let Err(e) = configured { let _ = fs::remove_dir(&slice); return Err(e); }
        Ok(slice)
    }
    pub fn attach_pid_with_mode(&self, id: &str, mode: NetworkMode, pid: u32) -> io::Result<()> {
        Self::validate_id(id)?; let slice=self.slice_path(id,mode); require_cgroup2(&slice)?;
        fs::write(slice.join("cgroup.procs"), pid.to_string())?;
        if !fs::read_to_string(slice.join("cgroup.procs"))?.lines().any(|s| s == pid.to_string()) { return Err(invalid("PID membership could not be verified")); } Ok(())
    }
    pub fn attach_pid(&self,id: &str,pid:u32)->io::Result<()> { self.attach_pid_with_mode(id,NetworkMode::Host,pid) }
    pub fn measurements(&self,id:&str)->io::Result<(f64,u64)> {
        Self::validate_id(id)?;let slice=self.slice_path(id,NetworkMode::Host);require_cgroup2(&slice)?;
        let stat=fs::read_to_string(slice.join("cpu.stat"))?;
        let usage=stat.lines().find_map(|l|l.strip_prefix("usage_usec ")).ok_or_else(||invalid("Missing CPU usage"))?.parse::<u64>().map_err(|_|invalid("Invalid CPU usage"))?;
        let peak=fs::read_to_string(slice.join("memory.peak"))?.trim().parse().map_err(|_|invalid("Invalid memory peak"))?;
        Ok((usage as f64 / 1_000_000.0,peak))
    }
    /// Kill all descendants and confirm the cgroup is empty before capacity is released.
    pub fn destroy_lease_slice(&self,id:&str)->io::Result<()> {
        Self::validate_id(id)?;let slice=self.slice_path(id,NetworkMode::Host);
        if !slice.exists(){return Ok(())} require_cgroup2(&slice)?;
        fs::write(slice.join("cgroup.kill"),"1")?;
        for _ in 0..100 {
            let events=fs::read_to_string(slice.join("cgroup.events"))?;
            if events.lines().any(|s|s=="populated 0") { return fs::remove_dir(&slice); }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        Err(invalid("Cgroup still populated; capacity must remain reserved"))
    }
    pub fn reconcile_and_clean_orphans(&self)->io::Result<usize> {
        require_cgroup2(&self.base_path)?;let mut count=0;
        for entry in fs::read_dir(self.base_path.join("host"))? {
            let entry=entry?;
            if entry.file_type()?.is_dir() { let id=entry.file_name().to_string_lossy().to_string();Self::validate_id(&id)?;self.destroy_lease_slice(&id)?;count+=1; }
        } Ok(count)
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn regular_files_are_never_kernel_enforcement(){ assert!(require_cgroup2(&std::env::temp_dir()).is_err()); }
    #[test] fn lease_paths_cannot_escape(){ for id in ["../escape","ls-../../escape","ls-x/y",""] {assert!(CgroupController::validate_id(id).is_err());} }
}
