use serde::{Deserialize, Serialize};

use crate::layout::{Guid, Layout, Partition};
use crate::units::{Lba, Sectors, format_bytes};

/// One step of a plan. Each operation ends with the partition table describing its result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Remove a partition's entry from the table, freeing its space. The data is left on the disk,
    /// unreferenced, until something writes over it, so a mistaken delete is recoverable.
    Delete { partition: Guid },
}

impl Operation {
    /// The partition this operation changes.
    pub fn partition(&self) -> Guid {
        match self {
            Operation::Shrink { partition, .. }
            | Operation::Move { partition, .. }
            | Operation::Grow { partition, .. }
            | Operation::RemoveSwap { partition }
            | Operation::Delete { partition } => *partition,
            Operation::RecreateSwap { partition } => partition.unique_guid,
        }
    }

    /// The step in plain words, as the CLI prints it and the GUI lists it.
    pub fn describe(&self, layout: &Layout) -> String {
        let sector = layout.geometry.sector_size;
        let size = |sectors: Sectors| format_bytes(sectors.to_bytes(sector));
        let at = |lba: Lba| size(Sectors(lba.0));
        let name = match self {
            Operation::RecreateSwap { partition } => partition.label(),
            _ => layout
                .partition(self.partition())
                .map_or_else(|| self.partition().to_string(), Partition::label),
        };
        match self {
            Operation::Shrink { len, .. } => format!("Shrink {name} to {}", size(*len)),
            Operation::Move {
                to, copy_estimate, ..
            } => format!(
                "Move {name} to {} (copies about {})",
                at(*to),
                size(*copy_estimate)
            ),
            Operation::Grow { len, .. } => format!("Grow {name} to {}", size(*len)),
            Operation::RemoveSwap { .. } => format!("Set aside swap {name}"),
            Operation::RecreateSwap { partition } => {
                format!("Recreate swap {name} at {}", at(partition.extent.start))
            }
            Operation::Delete { .. } => format!("Remove {name}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
            Operation::RemoveSwap { partition } | Operation::Delete { partition } => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Content;
    use crate::test_support::{at, guid, layout, mib, partition};

    fn named_layout() -> Layout {
        let mut windows = partition(3, Content::Ntfs, 1, 1024);
        windows.name = "Windows".to_string();
        layout(4096, vec![windows, partition(6, Content::Swap, 2048, 512)])
    }

    #[test]
    fn steps_name_a_labelled_partition_by_its_label() {
        let layout = named_layout();
        let shrink = Operation::Shrink {
            partition: guid(3),
            len: mib(512),
        };
        assert_eq!(
            shrink.describe(&layout),
            "Shrink partition 3 \"Windows\" to 512.0 MiB"
        );
    }

    #[test]
    fn steps_name_an_unlabelled_partition_by_its_contents() {
        let layout = named_layout();
        assert_eq!(
            Operation::RemoveSwap { partition: guid(6) }.describe(&layout),
            "Set aside swap partition 6 (swap)"
        );
    }

    #[test]
    fn a_move_says_where_it_lands_and_how_much_it_copies() {
        let layout = named_layout();
        let step = Operation::Move {
            partition: guid(3),
            from: at(1),
            to: at(1536),
            len: mib(1024),
            copy_estimate: mib(500),
        };
        assert_eq!(
            step.describe(&layout),
            "Move partition 3 \"Windows\" to 1.5 GiB (copies about 500.0 MiB)"
        );
    }

    #[test]
    fn a_recreated_swap_is_named_even_after_it_left_the_table() {
        let mut layout = named_layout();
        let swap = layout.partitions.remove(1);
        let mut moved = swap.clone();
        moved.extent.start = at(3072);
        assert_eq!(
            Operation::RecreateSwap { partition: moved }.describe(&layout),
            "Recreate swap partition 6 (swap) at 3.0 GiB"
        );
    }
}
