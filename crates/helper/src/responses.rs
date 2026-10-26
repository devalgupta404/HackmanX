//! The JSON the helper returns. Every reply is either a value or the refusals that stopped it,
//! each with a fix, so the GUI can show them all at once.

use bootshift_core::layout::{Layout, Target};
use bootshift_core::plan::Operation;
use bootshift_core::refusal::Refusal;
use bootshift_engine::preflight;
use bootshift_engine::service::{self, Inspected, ServiceError};
use bootshift_fs::probe::DiskInfo;
use serde::Serialize;

#[derive(Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Reply<T> {
    Ok { value: T },
    Refused { refusals: Vec<Refusal> },
}

#[derive(Serialize)]
pub struct DiskSummary {
    pub path: String,
    pub size: u64,
    pub transport: Option<String>,
    pub removable: bool,
    pub partition_table: Option<String>,
}

#[derive(Serialize)]
pub struct Probed {
    pub disk: String,
    pub size: u64,
    pub layout: Layout,
    /// The layout as it is now, for the GUI to start editing from.
    pub target: Target,
}

#[derive(Serialize)]
pub struct Planned {
    pub operations: Vec<Operation>,
    pub copy_bytes: u64,
    /// Steps this build can't do safely yet; the plan can't be applied while any remain.
    pub unavailable: Vec<Refusal>,
}

#[derive(Serialize)]
pub struct Applied {
    pub restart_to_begin: bool,
}

pub fn disk_summaries(disks: &[DiskInfo]) -> Vec<DiskSummary> {
    disks
        .iter()
        .map(|disk| DiskSummary {
            path: disk.path.display().to_string(),
            size: disk.size,
            transport: disk.transport.clone(),
            removable: disk.removable,
            partition_table: disk.partition_table.clone(),
        })
        .collect()
}

pub fn probed(inspected: &Inspected) -> Probed {
    Probed {
        disk: inspected.disk.path.display().to_string(),
        size: inspected.disk.size,
        layout: inspected.layout.clone(),
        target: service::current_target(&inspected.layout),
    }
}

pub fn parse_target(json: &str) -> Result<Target, ServiceError> {
    serde_json::from_str(json).map_err(|error| {
        ServiceError::Refused(vec![Refusal::new(
            format!("The target layout can't be read: {error}."),
            "Arrange the partitions again.",
        )])
    })
}

pub fn planned(layout: &Layout, target: &Target) -> Result<Planned, ServiceError> {
    let plan = service::plan_target(layout, target)?;
    let sector_size = layout.geometry.sector_size;
    Ok(Planned {
        copy_bytes: plan.sectors_to_copy().to_bytes(sector_size),
        unavailable: preflight::unavailable_operations(layout, &plan),
        operations: plan.operations,
    })
}

/// Turns a result into reply JSON. Refusals are an expected answer, not an error; anything
/// else is returned as `Err` for D-Bus to report as a failure.
pub fn reply<T: Serialize>(result: Result<T, ServiceError>) -> Result<String, String> {
    let reply = match result {
        Ok(value) => Reply::Ok { value },
        Err(ServiceError::Refused(refusals)) => Reply::Refused { refusals },
        Err(error) => return Err(error.to_string()),
    };
    serde_json::to_string(&reply).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use bootshift_core::layout::{Content, Extent, Geometry, Guid, Partition};
    use bootshift_core::units::{Lba, Sectors};

    use super::*;

    fn layout() -> Layout {
        Layout {
            geometry: Geometry {
                sector_size: 512,
                usable: Extent::new(Lba(2048), Sectors(200_000)),
                alignment: Sectors(2048),
            },
            partitions: vec![Partition {
                number: 1,
                unique_guid: Guid([1; 16]),
                type_guid: Guid([2; 16]),
                name: "data".to_string(),
                attributes: 0,
                extent: Extent::new(Lba(2048), Sectors(20_480)),
                content: Content::Ext4,
                used: Sectors(10_240),
                min_len: Some(Sectors(10_240)),
            }],
        }
    }

    #[test]
    fn a_reachable_target_replies_with_the_steps() {
        let layout = layout();
        let mut target = service::current_target(&layout);
        target.insert(Guid([1; 16]), Extent::new(Lba(4096), Sectors(20_480)));

        let json = reply(planned(&layout, &target)).unwrap();

        assert!(json.starts_with(r#"{"result":"ok","value":{"operations":[{"Move""#));
        assert!(json.contains(r#""copy_bytes":5242880"#));
    }

    #[test]
    fn a_refused_target_replies_with_its_refusals() {
        let layout = layout();
        let mut target = service::current_target(&layout);
        target.insert(Guid([1; 16]), Extent::new(Lba(1), Sectors(20_480)));

        let json = reply(planned(&layout, &target)).unwrap();

        assert!(json.starts_with(r#"{"result":"refused","refusals":[{"reason":"#));
    }

    #[test]
    fn unreadable_target_json_is_a_refusal_with_a_fix() {
        let json = reply(parse_target("{not json")).unwrap();
        assert!(json.contains(r#""result":"refused""#));
        assert!(json.contains("Arrange the partitions again."));
    }

    #[test]
    fn other_failures_are_errors() {
        let result: Result<(), ServiceError> = Err(ServiceError::UnknownDisk("/dev/nope".into()));
        assert_eq!(
            reply(result).unwrap_err(),
            "/dev/nope is not a disk the system knows about"
        );
    }
}
