use nomos_core::budget::PhysicalHostResources;
use sysinfo::{Disks, System};
#[cfg(target_os = "linux")]
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedHardware {
    pub resources: PhysicalHostResources,
    pub has_gpu: bool,
    pub gpu_details: Vec<String>,
    pub has_npu: bool,
    pub npu_details: Vec<String>,
}

pub fn probe_host() -> DetectedHardware {
    let mut sys = System::new_all();
    sys.refresh_all();

    let total_cores = sys.cpus().len();
    let total_memory_bytes = sys.total_memory();
    let total_swap_bytes = sys.total_swap();

    let disks = Disks::new_with_refreshed_list();
    let total_storage_bytes = disks.iter().map(|d| d.total_space()).sum();

    #[allow(unused_mut, unused_assignments)]
    let mut has_gpu = false;
    let mut gpu_details = Vec::new();
    #[allow(unused_mut, unused_assignments)]
    let mut has_npu = false;
    let mut npu_details = Vec::new();

    // Probe Linux DRM GPU devices
    #[cfg(target_os = "linux")]
    {
        if Path::new("/dev/dri/renderD128").exists() {
            has_gpu = true;
            gpu_details.push("Linux DRM Render Node: /dev/dri/renderD128".to_string());
        } else if Path::new("/dev/dri/card0").exists() {
            has_gpu = true;
            gpu_details.push("Linux DRM Card Node: /dev/dri/card0".to_string());
        }

        if Path::new("/dev/accel/accel0").exists() {
            has_npu = true;
            npu_details.push("Linux NPU Accelerator: /dev/accel/accel0".to_string());
        }
    }

    // On macOS, Apple Silicon GPU and Neural Engine are unified
    #[cfg(target_os = "macos")]
    {
        has_gpu = true;
        gpu_details.push("Apple Silicon Metal GPU (Unified Memory)".to_string());
        has_npu = true;
        npu_details.push("Apple Neural Engine (ANE)".to_string());
    }

    DetectedHardware {
        resources: PhysicalHostResources {
            total_cores,
            total_memory_bytes,
            total_swap_bytes,
            total_storage_bytes,
        },
        has_gpu,
        gpu_details,
        has_npu,
        npu_details,
    }
}
