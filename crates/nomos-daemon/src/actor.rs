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
    ) -> (ArbiterHandle, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(256);
        let scheduler = Scheduler::new(pool);
        let initial_snapshot = scheduler.snapshot();
        let latest_snapshot = Arc::new(RwLock::new(initial_snapshot));

        let cgroups = CgroupController::new();
        // Clean up orphan cgroups from prior runs at boot
        let orphans_cleaned = cgroups.reconcile_and_clean_orphans();
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

        (
            ArbiterHandle {
                tx,
                latest_snapshot,
            },
            handle,
        )
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
                let lease = Lease::new(lease_req);
                let adm = self.scheduler.request_lease(lease);

                if let nomos_core::scheduler::AdmissionResult::Granted(ref l) = adm {
                    // Create cgroup slice if available
                    if let Ok(slice) = self.cgroups.create_lease_slice(
                        &l.id,
                        l.request.req_cpu,
                        l.request.req_memory_bytes,
                        None,
                        l.request.network_mode,
                    ) {
                        info!("Created cgroup slice at {:?}", slice);
                    }
                }

                ArbiterResponse::from_admission(adm)
            }
            ArbiterRequest::Heartbeat { lease_id } => {
                let success = self.scheduler.record_heartbeat(&lease_id);
                ArbiterResponse::HeartbeatAck { success }
            }
            ArbiterRequest::QueryLease { lease_id } => {
                match self.scheduler.get_lease(&lease_id) {
                    Some((l, None)) => ArbiterResponse::LeaseGranted(l),
                    Some((l, Some(pos))) => ArbiterResponse::LeaseQueued {
                        lease: l,
                        position: pos,
                        reason: "Waiting in queue for resources or DAG dependencies".to_string(),
                    },
                    None => ArbiterResponse::Error(format!("Lease {} not found", lease_id)),
                }
            }
            ArbiterRequest::ReleaseLease { lease_id } => {
                let (released_opt, newly_granted) = self.scheduler.release_lease(&lease_id);
                let _ = self.cgroups.destroy_lease_slice(&lease_id);

                // Record accounting with true worker name and measured runtime
                let now = Utc::now();
                if let Some(l) = released_opt {
                    let started = l.granted_at.unwrap_or(l.created_at);
                    let dur = l.actual_runtime_seconds;
                    let cpu_secs = dur * l.request.req_cpu;
                    let rec = AuditRecord {
                        lease_id: l.id.clone(),
                        worker_id: l.request.worker_id.clone(),
                        tenant: l.request.tenant.clone(),
                        started_at: started,
                        finished_at: now,
                        duration_seconds: dur,
                        cpu_core_seconds: cpu_secs,
                        peak_memory_bytes: l.request.req_memory_bytes,
                        gpu_seconds: if !l.request.devices.is_empty() { dur } else { 0.0 },
                        net_egress_bytes: 0,
                        final_state: format!("{:?}", l.state),
                    };
                    self.ledger.record(rec);
                }

                for granted in &newly_granted {
                    let _ = self.cgroups.create_lease_slice(
                        &granted.id,
                        granted.request.req_cpu,
                        granted.request.req_memory_bytes,
                        None,
                        granted.request.network_mode,
                    );
                }

                ArbiterResponse::ReleasedAck {
                    newly_granted_count: newly_granted.len(),
                }
            }
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

    async fn on_tick(&mut self) {
        let now = Utc::now();

        // 1. Check dead-man's switch heartbeats (15s timeout)
        let hung = self.scheduler.check_heartbeats(now, 15);
        for id in hung {
            warn!("Dead-man's switch triggered for hung lease {}, auto-reclaiming", id);
            let (_rel, newly_granted) = self.scheduler.release_lease(&id);
            let _ = self.cgroups.destroy_lease_slice(&id);
            for g in newly_granted {
                let _ = self.cgroups.create_lease_slice(
                    &g.id,
                    g.request.req_cpu,
                    g.request.req_memory_bytes,
                    None,
                    g.request.network_mode,
                );
            }
        }

        // 2. Check TTL lease expirations
        let expired = self.scheduler.check_expirations(now);
        for id in expired {
            info!("Lease {} expired by TTL, auto-releasing", id);
            let (_rel, newly_granted) = self.scheduler.release_lease(&id);
            let _ = self.cgroups.destroy_lease_slice(&id);
            for g in newly_granted {
                let _ = self.cgroups.create_lease_slice(
                    &g.id,
                    g.request.req_cpu,
                    g.request.req_memory_bytes,
                    None,
                    g.request.network_mode,
                );
            }
        }
    }
}
