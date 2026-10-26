//! Runs a plan in the recovery system, one small step at a time, committing every step to the
//! journal. After a power cut the same plan resumes from the journal alone, so every step is
//! safe to repeat.
//!
//! Steps within each operation:
//!
//! | Operation | Steps |
//! |---|---|
//! | Shrink | shrink the filesystem (guarded), write the table |
//! | Move | record used ranges, copy chunks, verify the destination, fix NTFS boot sectors, write the table |
//! | Grow | write the table, grow the filesystem |
//! | RemoveSwap | write the table |
//! | RecreateSwap | write the table, write the swap signature |

use bootshift_core::journal::{MAX_SCOPE_RANGES, Progress, Stage};
use bootshift_core::layout::{Content, Guid, Layout, Partition};
use bootshift_core::plan::{ApplyError, Operation};
use bootshift_core::scope::CopyScope;
use bootshift_disk::{BlockDevice, DiskError, GptError, GptImage, Journal, JournalError};
use bootshift_fs::run::RunError;

use crate::copier::{CopyError, CopyOptions, MoveJob, move_range, verify_move};
use crate::ntfs_boot::fix_hidden_sectors;
use crate::run_plan::RunPlan;

/// The filesystem tools and the kernel, as the executor sees them. The real implementation
/// runs tools on partition device nodes; tests use a fake.
pub trait Filesystems {
    /// `resized_by_us` says whether an earlier operation of this run resized the filesystem,
    /// which leaves an NTFS volume marked for checking.
    fn used_ranges(
        &mut self,
        partition: &Partition,
        resized_by_us: bool,
    ) -> Result<CopyScope, RunError>;
    /// Shrinks the filesystem to `len` bytes inside a guarded run, so a power cut part way
    /// through rolls back and the shrink simply runs again.
    fn shrink(&mut self, partition: &Partition, len: u64) -> Result<(), RunError>;
    /// Grows the filesystem to fill its (already grown) partition.
    fn grow(&mut self, partition: &Partition) -> Result<(), RunError>;
    fn recreate_swap(&mut self, partition: &Partition) -> Result<(), RunError>;
    fn check(&mut self, partition: &Partition, resized_by_us: bool) -> Result<(), RunError>;
    fn refresh_kernel_view(&mut self) -> Result<(), RunError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error(transparent)]
    Copy(#[from] CopyError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    Table(#[from] GptError),
    #[error(transparent)]
    Disk(#[from] DiskError),
    #[error(transparent)]
    Tool(#[from] RunError),
    #[error("the plan doesn't fit the layout it was made for: {0}")]
    Plan(#[from] ApplyError),
    #[error("the moved data of operation {0} no longer matches what was copied")]
    VerifyFailed(u32),
    #[error("the journal points at operation {0}, step {1}, which the plan doesn't have")]
    BadProgress(u32, u8),
}

/// Runs `run` from `resume_from` (`None` for a fresh journal) until the disk holds the target
/// layout and every changed filesystem has checked clean. Leaves the journal at
/// [`Stage::Finishing`], for the boot layer to restore normal booting.
pub fn execute<D: BlockDevice + ?Sized, F: Filesystems>(
    disk: &mut D,
    journal: &mut Journal,
    filesystems: &mut F,
    run: &RunPlan,
    resume_from: Option<Progress>,
    options: &CopyOptions,
) -> Result<(), ExecError> {
    let mut executor = Executor {
        disk,
        journal,
        filesystems,
        run,
        layouts: layouts_after_each_operation(run)?,
        removable: removed_swaps(run),
        options,
    };
    let mut progress = resume_from.unwrap_or_else(|| stage(Stage::Prepared));
    loop {
        let operations = run.plan.operations.len() as u32;
        progress = match progress.stage {
            Stage::Prepared => executor.commit(step(0, 0))?,
            Stage::Running if progress.operation < operations => executor.advance(&progress)?,
            Stage::Running => executor.commit(stage(Stage::Verifying))?,
            Stage::Verifying => {
                executor.check_changed_filesystems()?;
                executor.commit(stage(Stage::Finishing))?
            }
            Stage::Finishing | Stage::Done => return Ok(()),
        };
    }
}

struct Executor<'a, D: ?Sized, F> {
    disk: &'a mut D,
    journal: &'a mut Journal,
    filesystems: &'a mut F,
    run: &'a RunPlan,
    /// `layouts[k]` is the partition table once the first `k` operations are done.
    layouts: Vec<Layout>,
    removable: Vec<Guid>,
    options: &'a CopyOptions,
}

impl<D: BlockDevice + ?Sized, F: Filesystems> Executor<'_, D, F> {
    fn commit(&mut self, progress: Progress) -> Result<Progress, ExecError> {
        self.journal.commit(self.disk, &progress)?;
        Ok(progress)
    }

    /// Performs the step `progress` points at and commits the next one.
    fn advance(&mut self, progress: &Progress) -> Result<Progress, ExecError> {
        let index = progress.operation;
        let done = progress.steps_done;
        let operation = self
            .run
            .plan
            .operations
            .get(index as usize)
            .ok_or(ExecError::BadProgress(index, done))?;
        let next_operation = step(index + 1, 0);

        let next = match (operation, done) {
            (Operation::Shrink { partition, len }, 0) => {
                let before = self.partition_before(index, *partition)?;
                let bytes = len.to_bytes(self.sector_size());
                self.filesystems.shrink(&before, bytes)?;
                step(index, 1)
            }
            (Operation::Shrink { .. }, 1) => {
                self.write_table(index)?;
                next_operation
            }

            (Operation::Move { partition, .. }, 0) => {
                let before = self.partition_before(index, *partition)?;
                let scope = self.measure_scope(&before, self.resized_before(index, *partition))?;
                self.journal.record_scope(self.disk, index, &scope)?;
                step(index, 1)
            }
            (Operation::Move { from, to, len, .. }, 1) => {
                let scope = self.journal.recorded_scope(self.disk, index)?;
                let sector_size = u64::from(self.sector_size());
                let job = MoveJob {
                    source: from.0 * sector_size,
                    destination: to.0 * sector_size,
                    len: len.0 * sector_size,
                    scope,
                };
                let outcome = move_range(
                    self.disk,
                    self.journal,
                    job.clone(),
                    progress,
                    self.options.clone(),
                )?;
                if verify_move(self.disk, job, self.options)? != outcome.hash_chain {
                    return Err(ExecError::VerifyFailed(index));
                }
                step(index, 2)
            }
            (Operation::Move { partition, .. }, 2) => {
                let after = self.partition_after(index, *partition)?;
                if after.content == Content::Ntfs {
                    fix_hidden_sectors(self.disk, after.extent)?;
                }
                step(index, 3)
            }
            (Operation::Move { .. }, 3) => {
                self.write_table(index)?;
                next_operation
            }

            (Operation::Grow { .. }, 0) => {
                self.write_table(index)?;
                step(index, 1)
            }
            (Operation::Grow { partition, .. }, 1) => {
                let after = self.partition_after(index, *partition)?;
                self.filesystems.grow(&after)?;
                next_operation
            }

            (Operation::RemoveSwap { .. }, 0) => {
                self.write_table(index)?;
                next_operation
            }

            (Operation::RecreateSwap { .. }, 0) => {
                self.write_table(index)?;
                step(index, 1)
            }
            (Operation::RecreateSwap { partition }, 1) => {
                self.filesystems.recreate_swap(partition)?;
                next_operation
            }

            _ => return Err(ExecError::BadProgress(index, done)),
        };
        self.commit(next)
    }

    fn measure_scope(
        &mut self,
        partition: &Partition,
        resized_by_us: bool,
    ) -> Result<CopyScope, ExecError> {
        if !partition.content.copies_used_only() {
            return Ok(CopyScope::Everything);
        }
        let scope = self.filesystems.used_ranges(partition, resized_by_us)?;
        Ok(scope.coalesced(MAX_SCOPE_RANGES))
    }

    /// Writes the table as it is once operation `index` is done, rebuilt from whichever copy
    /// is currently valid, then tells the kernel.
    fn write_table(&mut self, index: u32) -> Result<(), ExecError> {
        let target = &self.layouts[index as usize + 1];
        GptImage::read(self.disk)?
            .with_partitions(&target.partitions, &self.removable)?
            .write(self.disk)?;
        self.filesystems.refresh_kernel_view()?;
        Ok(())
    }

    fn check_changed_filesystems(&mut self) -> Result<(), ExecError> {
        let changed: Vec<Guid> = self
            .run
            .plan
            .operations
            .iter()
            .map(Operation::partition)
            .collect();
        let Some(last) = self.layouts.last() else {
            return Ok(());
        };
        let operations = self.run.plan.operations.len() as u32;
        for partition in last.partitions.clone() {
            if partition.content.copies_used_only() && changed.contains(&partition.unique_guid) {
                let resized = self.resized_before(operations, partition.unique_guid);
                self.filesystems.check(&partition, resized)?;
            }
        }
        Ok(())
    }

    /// Whether any operation before `index` shrank or grew this partition's filesystem.
    fn resized_before(&self, index: u32, guid: Guid) -> bool {
        self.run.plan.operations[..index as usize]
            .iter()
            .any(|operation| {
                matches!(operation,
                Operation::Shrink { partition, .. } | Operation::Grow { partition, .. }
                    if *partition == guid)
            })
    }

    fn partition_before(&self, index: u32, guid: Guid) -> Result<Partition, ExecError> {
        self.partition_in(index as usize, guid)
    }

    fn partition_after(&self, index: u32, guid: Guid) -> Result<Partition, ExecError> {
        self.partition_in(index as usize + 1, guid)
    }

    fn partition_in(&self, layout: usize, guid: Guid) -> Result<Partition, ExecError> {
        self.layouts
            .get(layout)
            .and_then(|l| l.partition(guid))
            .cloned()
            .ok_or(ExecError::Plan(ApplyError::UnknownPartition(guid)))
    }

    fn sector_size(&self) -> u32 {
        self.run.layout.geometry.sector_size
    }
}

fn layouts_after_each_operation(run: &RunPlan) -> Result<Vec<Layout>, ApplyError> {
    let mut layouts = vec![run.layout.clone()];
    for operation in &run.plan.operations {
        let mut next = layouts[layouts.len() - 1].clone();
        next.apply(operation)?;
        layouts.push(next);
    }
    Ok(layouts)
}

fn removed_swaps(run: &RunPlan) -> Vec<Guid> {
    run.plan
        .operations
        .iter()
        .filter_map(|operation| match operation {
            Operation::RemoveSwap { partition } => Some(*partition),
            _ => None,
        })
        .collect()
}

fn step(operation: u32, steps_done: u8) -> Progress {
    Progress {
        stage: Stage::Running,
        operation,
        steps_done,
        next_chunk: 0,
        hash_chain: [0; 32],
    }
}

fn stage(stage: Stage) -> Progress {
    Progress {
        stage,
        ..step(0, 0)
    }
}
