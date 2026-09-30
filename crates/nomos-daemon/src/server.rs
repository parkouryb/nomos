use nomos_core::accounting::{AccountingSummary, AuditRecord};
use nomos_core::budget::BudgetConfig;
use nomos_core::lease::{Lease, LeaseRequest};
use nomos_core::scheduler::{AdmissionResult, SchedulerSnapshot};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArbiterRequest {
    AcquireLease(LeaseRequest),
    Heartbeat { lease_id: String },
    ReleaseLease { lease_id: String },
    QueryLease { lease_id: String },
    GetStatus,
    GetAccounting {
        limit: usize,
        #[serde(default)]
        days: Option<u32>,
    },
    SetBudget(BudgetConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArbiterResponse {
    LeaseGranted(Lease),
    LeaseQueued {
        lease: Lease,
        position: usize,
        reason: String,
    },
    HeartbeatAck { success: bool },
    ReleasedAck { newly_granted_count: usize },
    Status(SchedulerSnapshot),
    Accounting {
        summary: AccountingSummary,
        recent: Vec<AuditRecord>,
    },
    BudgetUpdated,
    Error(String),
}

impl ArbiterResponse {
    pub fn from_admission(result: AdmissionResult) -> Self {
        match result {
            AdmissionResult::Granted(l) => ArbiterResponse::LeaseGranted(l),
            AdmissionResult::Queued { lease, queue_position, reason } => {
                ArbiterResponse::LeaseQueued {
                    lease,
                    position: queue_position,
                    reason,
                }
            }
            AdmissionResult::Rejected { reason } => ArbiterResponse::Error(reason),
        }
    }
}
