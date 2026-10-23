use crate::server::{ArbiterRequest, ArbiterResponse};
use chrono::Utc;
use nomos_core::accounting::{AuditLedger, AuditRecord};
use nomos_core::budget::NomosPool;
use nomos_core::lease::Lease;
use nomos_core::scheduler::Scheduler;
use nomos_sys::cgroup::CgroupController;
use nomos_sys::probe::probe_host;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{error, info, warn};

pub type ArbiterMessage = (ArbiterRequest, oneshot::Sender<ArbiterResponse>);

pub struct ArbiterActor {
    scheduler: Scheduler,
    ledger: AuditLedger,
    cgroups: CgroupController,
    rx: mpsc::Receiver<ArbiterMessage>,
}

#[derive(Clone)]
pub struct ArbiterHandle {
    tx: mpsc::Sender<ArbiterMessage>,
    latest_snapshot: Arc<RwLock<nomos_core::scheduler::SchedulerSnapshot>>,
}

impl ArbiterHandle {
    pub async fn send(&self, req: ArbiterRequest) -> Result<ArbiterResponse, anyhow::Error> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx.send((req, reply_tx)).await.map_err(|_| anyhow::anyhow!("Arbiter actor has terminated"))?;
        let res = reply_rx.await.map_err(|_| anyhow::anyhow!("Arbiter reply sender dropped"))?;
        Ok(res)
    }

    pub async fn get_cached_snapshot(&self) -> nomos_core::scheduler::SchedulerSnapshot {
        self.latest_snapshot.read().await.clone()
    }
}

impl ArbiterActor {
    pub fn spawn(
        pool: NomosPool,
        audit_path: Option<String>,
        retention_days: u32,
    ) -> anyhow::Result<(ArbiterHandle, tokio::task::JoinHandle<()>)> {
        let (tx, rx) = mpsc::channel(256);
        let scheduler = Scheduler::new(pool);
        let initial_snapshot = scheduler.snapshot();
        let latest_snapshot = Arc::new(RwLock::new(initial_snapshot));

        let cgroups = CgroupController::new();
        // Clean up orphan cgroups from prior runs at boot
        cgroups.initialize()?;
        let orphans_cleaned = cgroups.reconcile_and_clean_orphans()?;
        if orphans_cleaned > 0 {
            info!("Boot reconciliation: Cleaned {} orphan cgroup slices", orphans_cleaned);
        }

        let ledger = AuditLedger::with_retention(audit_path, retention_days);

        let actor = Self {
            scheduler,
            ledger,
            cgroups,
            rx,
        };

        let snapshot_ref = latest_snapshot.clone();

        let handle = tokio::spawn(async move {
            actor.run(snapshot_ref).await;
        });

        Ok((
            ArbiterHandle {
                tx,
                latest_snapshot,
            },
            handle,
        ))
    }

    async fn run(mut self, snapshot_ref: Arc<RwLock<nomos_core::scheduler::SchedulerSnapshot>>) {
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(500));

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.on_tick().await;
                    // Update cached snapshot for fast read
                    let snap = self.scheduler.snapshot();
                    *snapshot_ref.write().await = snap;
                }
                msg = self.rx.recv() => {
                    match msg {
                        Some((req, reply)) => {
                            let resp = self.handle_request(req).await;
                            let _ = reply.send(resp);
                            let snap = self.scheduler.snapshot();
                            *snapshot_ref.write().await = snap;
                        }
                        None => {
                            info!("Arbiter actor channel closed, shutting down");
                            break;
                        }
                    }
                }
            }
        }
    }

    async fn handle_request(&mut self, req: ArbiterRequest) -> ArbiterResponse {
        match req {
            ArbiterRequest::AcquireLease(lease_req) => {
                if lease_req.network_mode != nomos_core::lease::NetworkMode::Host || !lease_req.devices.is_empty() || lease_req.req_scratch_bytes != 0 || lease_req.network_bandwidth_mbps.is_some() {
                    return ArbiterResponse::Error("Requested network/device/scratch enforcement is not implemented; use explicit host network and CPU/RAM only".into());
                }
                let adm = self.scheduler.request_lease(Lease::new(lease_req));
                match adm {
                    nomos_core::scheduler::AdmissionResult::Granted(l) => {
                        let id = l.id.clone();
                        self.enforce_grants(vec![l]);
                        match self.scheduler.get_lease(&id) {
                            Some((lease, None)) if lease.cgroup_path.is_some() => ArbiterResponse::LeaseGranted(lease),
                            _ => ArbiterResponse::Error("Kernel resource enforcement failed; lease was not granted".into()),
                        }
                    }
                    other => ArbiterResponse::from_admission(other),
                }
            }
            ArbiterRequest::Heartbeat { lease_id } => {
                let success = self.scheduler.record_heartbeat(&lease_id);
                ArbiterResponse::HeartbeatAck { success }
            }
            ArbiterRequest::QueryLease { lease_id } => {
                match self.scheduler.get_lease(&lease_id) {
                    Some((l, None)) if l.state.is_active() && l.cgroup_path.is_some() => ArbiterResponse::LeaseGranted(l),
                    Some((_, None)) => ArbiterResponse::Error("Lease is terminating or enforcement was not verified".into()),
                    Some((l, Some(pos))) => ArbiterResponse::LeaseQueued {
                        lease: l,
                        position: pos,
                        reason: "Waiting in queue for resources or DAG dependencies".to_string(),
                    },
                    None => ArbiterResponse::Error(format!("Lease {} not found", lease_id)),
                }
            }
            ArbiterRequest::ReleaseLease { lease_id } => self.finish(&lease_id, false),
            ArbiterRequest::FinishLease { lease_id, success } => self.finish(&lease_id, success),
            ArbiterRequest::GetStatus => {
                ArbiterResponse::Status(self.scheduler.snapshot())
            }
            ArbiterRequest::GetAccounting { limit, days } => {
                ArbiterResponse::Accounting {
                    summary: self.ledger.summary_window(days),
                    recent: self.ledger.recent_records_window(limit, days).to_vec(),
                }
            }
            ArbiterRequest::SetBudget(cfg) => {
                let host = probe_host();
                match NomosPool::from_config(&cfg, &host.resources) {
                    Ok(new_pool) => {
                        if new_pool.total_cores < self.scheduler.allocated_cpu() || new_pool.total_memory_bytes < self.scheduler.allocated_memory_bytes() {
                            return ArbiterResponse::Error("Budget is below active reservations; stop workloads first".into());
                        }
                        info!("Hot-reloading Nomos pool: {:?}", new_pool);
                        self.scheduler.set_pool(new_pool);
                        ArbiterResponse::BudgetUpdated
                    }
                    Err(e) => {
                        error!("Failed to update budget: {}", e);
                        ArbiterResponse::Error(e.to_string())
                    }
                }
            }
        }
    }

    fn enforce_grants(&mut self, grants: Vec<Lease>) {
        let mut pending = std::collections::VecDeque::from(grants);
        while let Some(l) = pending.pop_front() {
            match self.cgroups.create_lease_slice(&l.id, l.request.req_cpu, l.request.req_memory_bytes, None, l.request.network_mode) {
                Ok(path) => self.scheduler.set_enforced_path(&l.id, path.to_string_lossy().into_owned()),
                Err(err) => {
                    error!(lease_id=%l.id, "Kernel enforcement failed: {}", err);
                    // No worker has received this lease. Do not unlock dependent jobs.
                    let (_, next) = self.scheduler.finish_lease(&l.id, false);
                    pending.extend(next);
                }
            }
        }
    }

    fn finish(&mut self, id: &str, success: bool) -> ArbiterResponse {
        let metrics = self.cgroups.measurements(id).ok();
        // Never reclaim scheduling capacity while a process can still be alive.
        if let Err(err) = self.cgroups.destroy_lease_slice(id) {
            return ArbiterResponse::Error(format!("Cannot terminate lease {}; capacity remains reserved: {}", id, err));
        }
        let (released, next) = self.scheduler.finish_lease(id, success);
        if let Some(l) = released {
            let (cpu, memory) = metrics.unwrap_or((0.0, 0));
            self.ledger.record(AuditRecord {
                cpu_memory_measured: metrics.is_some(), gpu_network_measured: false,
                lease_id: l.id, worker_id: l.request.worker_id, tenant: l.request.tenant,
                started_at: l.granted_at.unwrap_or(l.created_at), finished_at: Utc::now(),
                duration_seconds: l.actual_runtime_seconds, cpu_core_seconds: cpu,
                peak_memory_bytes: memory, gpu_seconds: 0.0, net_egress_bytes: 0,
                final_state: format!("{:?}", l.state),
            });
        }
        let ids = next.iter().map(|l| l.id.clone()).collect::<Vec<_>>();
        self.enforce_grants(next);
        let count = ids.iter().filter(|id| self.scheduler.get_lease(id).is_some()).count();
        ArbiterResponse::ReleasedAck { newly_granted_count: count }
    }

    async fn on_tick(&mut self) {
        let now = Utc::now();
        let mut expired = self.scheduler.check_heartbeats(now, 15);
        expired.extend(self.scheduler.check_expirations(now));
        expired.sort(); expired.dedup();
        for id in expired {
            if let ArbiterResponse::Error(err) = self.finish(&id, false) { warn!("{}", err); }
        }
    }
}
