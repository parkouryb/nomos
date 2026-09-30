use crate::server::{ArbiterRequest, ArbiterResponse};
use futures::{SinkExt, StreamExt};
use nomos_core::accounting::{AccountingSummary, AuditRecord};
use nomos_core::lease::{Lease, LeaseRequest};
use nomos_core::scheduler::SchedulerSnapshot;
use std::path::Path;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LinesCodec};

pub struct NomosClient {
    framed: Framed<UnixStream, LinesCodec>,
}

impl NomosClient {
    pub async fn connect<P: AsRef<Path>>(socket_path: P) -> Result<Self, anyhow::Error> {
        let stream = UnixStream::connect(socket_path).await?;
        let framed = Framed::new(stream, LinesCodec::new());
        Ok(Self { framed })
    }

    pub async fn call(&mut self, req: ArbiterRequest) -> Result<ArbiterResponse, anyhow::Error> {
        let req_str = serde_json::to_string(&req)?;
        self.framed.send(req_str).await?;

        if let Some(line_res) = self.framed.next().await {
            let line = line_res?;
            let resp = serde_json::from_str::<ArbiterResponse>(&line)?;
            Ok(resp)
        } else {
            Err(anyhow::anyhow!("Arbiter socket closed unexpectedly"))
        }
    }

    pub async fn query_lease(&mut self, lease_id: &str) -> Result<ArbiterResponse, anyhow::Error> {
        self.call(ArbiterRequest::QueryLease { lease_id: lease_id.to_string() }).await
    }

    pub async fn acquire_lease(&mut self, req: LeaseRequest) -> Result<Lease, anyhow::Error> {
        self.acquire_lease_wait(req, std::time::Duration::from_secs(300)).await
    }

    #[allow(dead_code)]
    pub async fn acquire_lease_nowait(&mut self, req: LeaseRequest) -> Result<Lease, anyhow::Error> {
        match self.call(ArbiterRequest::AcquireLease(req)).await? {
            ArbiterResponse::LeaseGranted(l) => Ok(l),
            ArbiterResponse::LeaseQueued { lease: _, position, reason } => {
                Err(anyhow::anyhow!("Lease queued at position {}: {}", position, reason))
            }
            ArbiterResponse::Error(e) => Err(anyhow::anyhow!("Lease request rejected: {}", e)),
            other => Err(anyhow::anyhow!("Unexpected response from arbiter: {:?}", other)),
        }
    }

    pub async fn acquire_lease_wait(
        &mut self,
        req: LeaseRequest,
        timeout: std::time::Duration,
    ) -> Result<Lease, anyhow::Error> {
        match self.call(ArbiterRequest::AcquireLease(req)).await? {
            ArbiterResponse::LeaseGranted(l) => Ok(l),
            ArbiterResponse::LeaseQueued { lease, position, reason } => {
                let lease_id = lease.id;
                tracing::info!("Lease {} queued at position #{} ({}). Waiting for resources/DAG...", lease_id, position, reason);
                let start = std::time::Instant::now();
                loop {
                    if start.elapsed() > timeout {
                        return Err(anyhow::anyhow!("Timed out after {:?} waiting for lease {}", timeout, lease_id));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    match self.query_lease(&lease_id).await? {
                        ArbiterResponse::LeaseGranted(l) => {
                            tracing::info!("Lease {} is now GRANTED!", lease_id);
                            return Ok(l);
                        }
                        ArbiterResponse::LeaseQueued { .. } => {
                            // Still waiting
                        }
                        ArbiterResponse::Error(e) => {
                            return Err(anyhow::anyhow!("Lease failed while queued: {}", e));
                        }
                        other => {
                            return Err(anyhow::anyhow!("Unexpected response while polling lease: {:?}", other));
                        }
                    }
                }
            }
            ArbiterResponse::Error(e) => Err(anyhow::anyhow!("Lease request rejected: {}", e)),
            other => Err(anyhow::anyhow!("Unexpected response from arbiter: {:?}", other)),
        }
    }

    pub async fn heartbeat(&mut self, lease_id: &str) -> Result<bool, anyhow::Error> {
        match self.call(ArbiterRequest::Heartbeat { lease_id: lease_id.to_string() }).await? {
            ArbiterResponse::HeartbeatAck { success } => Ok(success),
            other => Err(anyhow::anyhow!("Unexpected response: {:?}", other)),
        }
    }

    pub async fn release_lease(&mut self, lease_id: &str) -> Result<(), anyhow::Error> {
        match self.call(ArbiterRequest::ReleaseLease { lease_id: lease_id.to_string() }).await? {
            ArbiterResponse::ReleasedAck { .. } => Ok(()),
            other => Err(anyhow::anyhow!("Unexpected response: {:?}", other)),
        }
    }

    pub async fn get_status(&mut self) -> Result<SchedulerSnapshot, anyhow::Error> {
        match self.call(ArbiterRequest::GetStatus).await? {
            ArbiterResponse::Status(s) => Ok(s),
            other => Err(anyhow::anyhow!("Unexpected response: {:?}", other)),
        }
    }

    #[allow(dead_code)]
    pub async fn get_accounting(&mut self, limit: usize) -> Result<(AccountingSummary, Vec<AuditRecord>), anyhow::Error> {
        self.get_accounting_window(limit, None).await
    }

    pub async fn get_accounting_window(
        &mut self,
        limit: usize,
        days: Option<u32>,
    ) -> Result<(AccountingSummary, Vec<AuditRecord>), anyhow::Error> {
        match self.call(ArbiterRequest::GetAccounting { limit, days }).await? {
            ArbiterResponse::Accounting { summary, recent } => Ok((summary, recent)),
            other => Err(anyhow::anyhow!("Unexpected response: {:?}", other)),
        }
    }
}
