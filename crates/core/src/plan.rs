use crate::layout::{Guid, Layout, Partition};
use crate::units::{Lba, Sectors};

/// One step of a plan. Each operation ends with the partition table describing its result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    /// Shrink the filesystem, then the partition, keeping its start.
    Shrink { partition: Guid, len: Sectors },
    /// Copy the partition's data to `to`, then point the table at it.
    Move {
        partition: Guid,
        from: Lba,
        to: Lba,
        len: Sectors,
        copy_estimate: Sectors,
    },
    /// Grow the partition, then the filesystem, keeping its start.
    Grow { partition: Guid, len: Sectors },
    /// Drop a swap partition from the table; it holds nothing worth copying.
    RemoveSwap { partition: Guid },
    /// Add the swap partition back at its target, with the same UUID and label.
    RecreateSwap { partition: Partition },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub operations: Vec<Operation>,
}

impl Plan {
    pub fn sectors_to_copy(&self) -> Sectors {
        self.operations
            .iter()
            .map(|op| match op {
                Operation::Move { copy_estimate, .. } => *copy_estimate,
                _ => Sectors(0),
            })
            .sum()
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("partition {0} is not in the layout")]
    UnknownPartition(Guid),
    #[error("operation does not match the current state of partition {0}")]
    Mismatch(Guid),
}

impl Layout {
    /// Updates the layout to what the partition table holds once `operation` has finished.
    pub fn apply(&mut self, operation: &Operation) -> Result<(), ApplyError> {
        match operation {
            Operation::Shrink { partition, len } => {
                let p = self.existing(*partition)?;
                ensure(*len <= p.extent.len, *partition)?;
                p.extent.len = *len;
            }
            Operation::Grow { partition, len } => {
                let p = self.existing(*partition)?;
                ensure(*len >= p.extent.len, *partition)?;
                p.extent.len = *len;
            }
            Operation::Move {
                partition,
                from,
                to,
                len,
                ..
            } => {
                let p = self.existing(*partition)?;
                ensure(p.extent.start == *from && p.extent.len == *len, *partition)?;
                p.extent.start = *to;
            }
            Operation::RemoveSwap { partition } => {
                let index = self
                    .partitions
                    .iter()
                    .position(|p| p.unique_guid == *partition)
                    .ok_or(ApplyError::UnknownPartition(*partition))?;
                self.partitions.remove(index);
            }
            Operation::RecreateSwap { partition } => {
                ensure(
                    self.partition(partition.unique_guid).is_none(),
                    partition.unique_guid,
                )?;
                self.partitions.push(partition.clone());
            }
        }
        Ok(())
    }

    fn existing(&mut self, guid: Guid) -> Result<&mut Partition, ApplyError> {
        self.partition_mut(guid)
            .ok_or(ApplyError::UnknownPartition(guid))
    }
}

fn ensure(condition: bool, guid: Guid) -> Result<(), ApplyError> {
    if condition {
        Ok(())
    } else {
        Err(ApplyError::Mismatch(guid))
    }
}
