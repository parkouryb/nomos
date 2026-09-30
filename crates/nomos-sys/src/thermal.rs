use sysinfo::Components;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThermalStatus {
    Cool,     // < 70°C
    Normal,   // 70 - 80°C
    Warm,     // 80 - 87°C (Warning, duty-cycle pacing suggested)
    Critical, // > 87°C (Immediate throttling required)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThermalTelemetry {
    pub package_temp_celsius: f32,
    pub max_temp_celsius: f32,
    pub status: ThermalStatus,
}

pub fn read_thermal_telemetry() -> ThermalTelemetry {
    let mut temps: Vec<f32> = Vec::new();

    // 1. Try reading Linux /sys/class/thermal directly
    if Path::new("/sys/class/thermal").exists() {
        if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
            for entry in entries.flatten() {
                let p = entry.path().join("temp");
                if p.exists() {
                    if let Ok(content) = fs::read_to_string(&p) {
                        if let Ok(milli) = content.trim().parse::<f32>() {
                            let deg = if milli > 1000.0 { milli / 1000.0 } else { milli };
                            if deg > 10.0 && deg < 120.0 {
                                temps.push(deg);
                            }
                        }
                    }
                }
            }
        }
    }

    // 2. Fallback to sysinfo components
    if temps.is_empty() {
        let components = Components::new_with_refreshed_list();
        for c in components.iter() {
            let t = c.temperature();
            if t > 10.0 && t < 120.0 {
                temps.push(t);
            }
        }
    }

    let (package_temp, max_temp) = if temps.is_empty() {
        (45.0, 45.0) // Safe default if hardware has no sensors
    } else {
        let sum: f32 = temps.iter().sum();
        let avg = sum / (temps.len() as f32);
        let max = *temps.iter().max_by(|a, b| a.partial_cmp(b).unwrap()).unwrap();
        (avg, max)
    };

    let status = if max_temp >= 88.0 {
        ThermalStatus::Critical
    } else if max_temp >= 80.0 {
        ThermalStatus::Warm
    } else if max_temp >= 68.0 {
        ThermalStatus::Normal
    } else {
        ThermalStatus::Cool
    };

    ThermalTelemetry {
        package_temp_celsius: package_temp,
        max_temp_celsius: max_temp,
        status,
    }
}
