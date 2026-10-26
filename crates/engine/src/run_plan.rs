//! What the normal system hands the recovery system: the layout it started from and the plan.
//! It is stored as JSON in the journal's plan record, so it can be read by eye when needed.

use bootshift_core::journal::PlanRecord;
use bootshift_core::layout::Layout;
use bootshift_core::plan::Plan;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPlan {
    /// The partition table as it was before the first operation.
    pub layout: Layout,
    pub plan: Plan,
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
