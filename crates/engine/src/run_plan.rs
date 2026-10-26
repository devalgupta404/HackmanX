//! What the normal system hands the recovery system: the layout it started from, the plan,
//! and what must be restored at the end. It is stored as JSON in the journal's plan record,
//! so it can be read by eye when needed.

use std::collections::BTreeMap;

use bootshift_core::journal::PlanRecord;
use bootshift_core::layout::{Guid, Layout};
use bootshift_core::plan::Plan;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPlan {
    /// The partition table as it was before the first operation.
    pub layout: Layout,
    pub plan: Plan,
    /// The UUID and label of each swap partition, read before the run, since a move may
    /// overwrite the old swap header before the swap is recreated.
    pub swaps: BTreeMap<Guid, SwapIdentity>,
    pub boot: BootRestore,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapIdentity {
    pub uuid: String,
    pub label: Option<String>,
}

/// How to put normal booting back once the run is done.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootRestore {
    pub esp_partition: u32,
    pub grub_vendor: String,
    pub original_boot_order: Vec<u16>,
}

impl RunPlan {
    pub fn to_record(&self, run_id: u64) -> Result<PlanRecord, serde_json::Error> {
        Ok(PlanRecord {
            run_id,
            payload: serde_json::to_vec_pretty(self)?,
        })
    }

    pub fn from_record(record: &PlanRecord) -> Result<RunPlan, serde_json::Error> {
        serde_json::from_slice(&record.payload)
    }
}
