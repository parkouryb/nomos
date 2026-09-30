use crate::budget::NomosPool;
use crate::dag::DagEngine;
use crate::lease::{DeviceType, Lease, LeaseState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AdmissionResult {
    Granted(Lease),
    Queued {
        lease: Lease,
        queue_position: usize,
        reason: String,
    },
    Rejected {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerSnapshot {
    pub pool: NomosPool,
    pub allocated_cpu: f64,
    pub allocated_memory_bytes: u64,
    pub allocated_scratch_bytes: u64,
    pub active_leases: Vec<Lease>,
    pub queued_leases: Vec<Lease>,
    pub active_devices: HashMap<String, String>, // device_name -> lease_id
}

pub struct Scheduler {
    pool: NomosPool,
    active_leases: HashMap<String, Lease>,
    wait_queue: VecDeque<Lease>,
    dag_engine: DagEngine,
    active_devices: HashMap<String, String>, // device identifier -> lease_id
}

impl Scheduler {
    pub fn new(pool: NomosPool) -> Self {
        Self {
            pool,
            active_leases: HashMap::new(),
            wait_queue: VecDeque::new(),
            dag_engine: DagEngine::new(),
            active_devices: HashMap::new(),
        }
    }

    pub fn pool(&self) -> &NomosPool {
        &self.pool
    }

    pub fn set_pool(&mut self, pool: NomosPool) {
        self.pool = pool;
    }

    pub fn allocated_cpu(&self) -> f64 {
        self.active_leases
            .values()
            .filter(|l| l.state.is_active())
            .map(|l| l.request.req_cpu)
            .sum()
    }

    pub fn allocated_memory_bytes(&self) -> u64 {
        self.active_leases
            .values()
            .filter(|l| l.state.is_active())
            .map(|l| l.request.req_memory_bytes)
            .sum()
    }

    pub fn allocated_scratch_bytes(&self) -> u64 {
        self.active_leases
            .values()
            .filter(|l| l.state.is_active())
            .map(|l| l.request.req_scratch_bytes)
            .sum()
    }

    pub fn request_lease(&mut self, mut lease: Lease) -> AdmissionResult {
        // 1. Check DAG dependencies
        if !self.dag_engine.register_task(&lease.request.worker_id, &lease.request.depends_on) {
            let pos = self.wait_queue.len() + 1;
            let reason = format!("Waiting on DAG dependencies: {:?}", lease.request.depends_on);
            self.wait_queue.push_back(lease.clone());
            return AdmissionResult::Queued {
                lease,
                queue_position: pos,
                reason,
            };
        }

        // 2. Check if request fits in total Nomos budget capacity
        if lease.request.req_cpu > self.pool.total_cores {
            return AdmissionResult::Rejected {
                reason: format!(
                    "Requested CPU ({:.1} cores) exceeds total Nomos budget capacity ({:.1} cores)",
                    lease.request.req_cpu, self.pool.total_cores
                ),
            };
        }
        if lease.request.req_memory_bytes > self.pool.total_memory_bytes {
            return AdmissionResult::Rejected {
                reason: format!(
                    "Requested RAM ({} B) exceeds total Nomos budget capacity ({} B)",
                    lease.request.req_memory_bytes, self.pool.total_memory_bytes
                ),
            };
        }

        // 3. Check current available headroom
        let current_cpu = self.allocated_cpu();
        let current_mem = self.allocated_memory_bytes();

        let cpu_fits = current_cpu + lease.request.req_cpu <= self.pool.total_cores;
        let mem_fits = current_mem + lease.request.req_memory_bytes <= self.pool.total_memory_bytes;
        let active_count_fits = self.active_leases.len() < self.pool.max_active_leases;

        // Check device availability
        let mut devices_fit = true;
        for dev in &lease.request.devices {
            let dev_key = match dev {
                DeviceType::Gpu(name) => format!("gpu:{}", name),
                DeviceType::Npu(name) => format!("npu:{}", name),
            };
            if self.active_devices.contains_key(&dev_key) {
                devices_fit = false;
                break;
            }
        }

        if cpu_fits && mem_fits && active_count_fits && devices_fit {
            // Admit immediately
            let ttl = lease.request.ttl_seconds;
            lease.mark_granted(ttl, None, None);

            // Bind devices
            for dev in &lease.request.devices {
                let dev_key = match dev {
                    DeviceType::Gpu(name) => format!("gpu:{}", name),
                    DeviceType::Npu(name) => format!("npu:{}", name),
                };
                self.active_devices.insert(dev_key, lease.id.clone());
            }

            self.active_leases.insert(lease.id.clone(), lease.clone());
            AdmissionResult::Granted(lease)
        } else {
            // Queue the lease
            let mut reason_parts = Vec::new();
            if !cpu_fits {
                reason_parts.push(format!("CPU needed: {:.1}, free: {:.1}", lease.request.req_cpu, self.pool.total_cores - current_cpu));
            }
            if !mem_fits {
                reason_parts.push(format!("RAM needed: {} B, free: {} B", lease.request.req_memory_bytes, self.pool.total_memory_bytes - current_mem));
            }
            if !devices_fit {
                reason_parts.push("Hardware devices currently busy".to_string());
            }
            let reason = reason_parts.join(" | ");

            self.enqueue_prioritized(lease.clone());
            let pos = self.wait_queue.iter().position(|l| l.id == lease.id).unwrap_or(0) + 1;

            AdmissionResult::Queued {
                lease,
                queue_position: pos,
                reason,
            }
        }
    }

    fn enqueue_prioritized(&mut self, lease: Lease) {
        // Higher priority tasks and shorter deadlines placed towards the front
        let mut inserted = false;
        for i in 0..self.wait_queue.len() {
            let existing = &self.wait_queue[i];
            if lease.request.priority > existing.request.priority {
                self.wait_queue.insert(i, lease.clone());
                inserted = true;
                break;
            } else if lease.request.priority == existing.request.priority {
                // If same priority, compare deadlines
                if let (Some(d_new), Some(d_old)) = (lease.request.deadline, existing.request.deadline) {
                    if d_new < d_old {
                        self.wait_queue.insert(i, lease.clone());
                        inserted = true;
                        break;
                    }
                }
            }
        }
        if !inserted {
            self.wait_queue.push_back(lease);
        }
    }

    pub fn record_heartbeat(&mut self, lease_id: &str) -> bool {
        if let Some(l) = self.active_leases.get_mut(lease_id) {
            l.record_heartbeat();
            true
        } else {
            false
        }
    }

    pub fn release_lease(&mut self, lease_id: &str) -> (Option<Lease>, Vec<Lease>) {
        let mut newly_granted = Vec::new();
        let mut released_opt = None;

        if let Some(mut lease) = self.active_leases.remove(lease_id) {
            lease.mark_completed();
            released_opt = Some(lease.clone());

            // Unbind devices
            self.active_devices.retain(|_, id| id != lease_id);

            // Mark completed in DAG and get newly unlocked workers
            let _unlocked = self.dag_engine.mark_completed(&lease.request.worker_id);

            // Try to admit queued tasks (Backfilling & Fair-share scheduling)
            let mut remaining_queue = VecDeque::new();

            while let Some(mut candidate) = self.wait_queue.pop_front() {
                // Check if DAG dependencies satisfied
                if !self.dag_engine.is_satisfied(&candidate.request.depends_on) {
                    remaining_queue.push_back(candidate);
                    continue;
                }

                let current_cpu = self.allocated_cpu();
                let current_mem = self.allocated_memory_bytes();

                let cpu_fits = current_cpu + candidate.request.req_cpu <= self.pool.total_cores;
                let mem_fits = current_mem + candidate.request.req_memory_bytes <= self.pool.total_memory_bytes;
                let active_count_fits = self.active_leases.len() < self.pool.max_active_leases;

                let mut devices_fit = true;
                for dev in &candidate.request.devices {
                    let dev_key = match dev {
                        DeviceType::Gpu(name) => format!("gpu:{}", name),
                        DeviceType::Npu(name) => format!("npu:{}", name),
                    };
                    if self.active_devices.contains_key(&dev_key) {
                        devices_fit = false;
                        break;
                    }
                }

                if cpu_fits && mem_fits && active_count_fits && devices_fit {
                    let ttl = candidate.request.ttl_seconds;
                    candidate.mark_granted(ttl, None, None);

                    for dev in &candidate.request.devices {
                        let dev_key = match dev {
                            DeviceType::Gpu(name) => format!("gpu:{}", name),
                            DeviceType::Npu(name) => format!("npu:{}", name),
                        };
                        self.active_devices.insert(dev_key, candidate.id.clone());
                    }

                    self.active_leases.insert(candidate.id.clone(), candidate.clone());
                    newly_granted.push(candidate);
                } else {
                    // Backfilling: Keep looking for smaller tasks in the queue that CAN fit!
                    remaining_queue.push_back(candidate);
                }
            }

            self.wait_queue = remaining_queue;
        }

        (released_opt, newly_granted)
    }

    pub fn check_heartbeats(&mut self, now: DateTime<Utc>, timeout_seconds: u64) -> Vec<String> {
        let mut hung = Vec::new();
        for (id, l) in &mut self.active_leases {
            if l.state == LeaseState::Running || l.state == LeaseState::Granted {
                if let Some(last) = l.last_heartbeat {
                    if (now - last).num_seconds() > timeout_seconds as i64 {
                        l.state = LeaseState::Hung;
                        hung.push(id.clone());
                    }
                }
            }
        }
        hung
    }

    pub fn check_expirations(&mut self, now: DateTime<Utc>) -> Vec<String> {
        let mut expired = Vec::new();
        for (id, l) in &mut self.active_leases {
            if l.state.is_active() {
                if let Some(exp) = l.expires_at {
                    if now > exp {
                        l.state = LeaseState::Expired;
                        expired.push(id.clone());
                    }
                }
            }
        }
        expired
    }

    pub fn get_lease(&self, lease_id: &str) -> Option<(Lease, Option<usize>)> {
        if let Some(l) = self.active_leases.get(lease_id) {
            return Some((l.clone(), None));
        }
        if let Some((pos, l)) = self.wait_queue.iter().enumerate().find(|(_, l)| l.id == lease_id) {
            return Some((l.clone(), Some(pos + 1)));
        }
        None
    }

    pub fn snapshot(&self) -> SchedulerSnapshot {
        SchedulerSnapshot {
            pool: self.pool.clone(),
            allocated_cpu: self.allocated_cpu(),
            allocated_memory_bytes: self.allocated_memory_bytes(),
            allocated_scratch_bytes: self.allocated_scratch_bytes(),
            active_leases: self.active_leases.values().cloned().collect(),
            queued_leases: self.wait_queue.iter().cloned().collect(),
            active_devices: self.active_devices.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lease::{LeaseRequest, Priority};

    #[test]
    fn test_admission_and_backfilling() {
        let pool = NomosPool {
            total_cores: 7.0,
            total_memory_bytes: 32 * 1024 * 1024 * 1024,
            host_reserve_memory_bytes: 8 * 1024 * 1024 * 1024,
            total_storage_bytes: 200 * 1024 * 1024 * 1024,
            max_active_leases: 10,
        };

        let mut sched = Scheduler::new(pool);

        // Task A: Heavy (6 Cores, 24GB) -> Should be Granted
        let req_a = LeaseRequest {
            worker_id: "worker-heavy-a".into(),
            tenant: "default".into(),
            req_cpu: 6.0,
            req_memory_bytes: 24 * 1024 * 1024 * 1024,
            req_scratch_bytes: 0,
            devices: vec![],
            network_mode: crate::lease::NetworkMode::Isolated,
            network_bandwidth_mbps: None,
            estimated_seconds: Some(180.0),
            deadline: None,
            depends_on: vec![],
            priority: Priority::Normal,
            ttl_seconds: 180,
        };
        let lease_a = Lease::new(req_a);
        let res_a = sched.request_lease(lease_a.clone());
        assert!(matches!(res_a, AdmissionResult::Granted(_)));

        // Task B: Another Heavy (4 Cores, 16GB) -> Should be Queued because only 1 Core, 8GB remaining
        let req_b = LeaseRequest {
            worker_id: "worker-heavy-b".into(),
            tenant: "default".into(),
            req_cpu: 4.0,
            req_memory_bytes: 16 * 1024 * 1024 * 1024,
            req_scratch_bytes: 0,
            devices: vec![],
            network_mode: crate::lease::NetworkMode::Isolated,
            network_bandwidth_mbps: None,
            estimated_seconds: Some(60.0),
            deadline: None,
            depends_on: vec![],
            priority: Priority::Normal,
            ttl_seconds: 60,
        };
        let lease_b = Lease::new(req_b);
        let res_b = sched.request_lease(lease_b);
        assert!(matches!(res_b, AdmissionResult::Queued { .. }));

        // Task C: Tiny Backfill (1 Core, 2GB) -> Should be Granted immediately (Backfilling)!
        let req_c = LeaseRequest {
            worker_id: "worker-fast-c".into(),
            tenant: "default".into(),
            req_cpu: 1.0,
            req_memory_bytes: 2 * 1024 * 1024 * 1024,
            req_scratch_bytes: 0,
            devices: vec![],
            network_mode: crate::lease::NetworkMode::Isolated,
            network_bandwidth_mbps: None,
            estimated_seconds: Some(2.0),
            deadline: None,
            depends_on: vec![],
            priority: Priority::High,
            ttl_seconds: 10,
        };
        let lease_c = Lease::new(req_c);
        let res_c = sched.request_lease(lease_c);
        assert!(matches!(res_c, AdmissionResult::Granted(_)));

        // Release Task A -> Task B should now be admitted automatically!
        let (rel_a, newly_granted) = sched.release_lease(&lease_a.id);
        assert!(rel_a.is_some());
        assert_eq!(newly_granted.len(), 1);
        assert_eq!(newly_granted[0].request.worker_id, "worker-heavy-b");
    }
}
