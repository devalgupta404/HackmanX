//! The read-only commands: probe, plan and check.

use std::path::Path;

use anyhow::{Context, bail};
use bootshift_core::layout::{Layout, Target};
use bootshift_core::plan::{Operation, Plan};
use bootshift_core::planner::{PlanError, plan as make_plan};
use bootshift_core::refusal::Refusal;
use bootshift_disk::{Access, GptImage, RealDevice};
use bootshift_engine::facts::{self, Environment};
use bootshift_engine::preflight;
use bootshift_engine::probe;
use bootshift_fs::probe::{DiskInfo, disks, find_disk};
use bootshift_fs::run::SystemRunner;

struct Inspected {
    disk: DiskInfo,
    layout: Layout,
}

pub fn probe(disk: &Path, target_template: bool) -> anyhow::Result<()> {
    let inspected = inspect(disk)?;
    if target_template {
        let target: Target = inspected
            .layout
            .partitions
            .iter()
            .map(|p| (p.unique_guid, p.extent))
            .collect();
        println!("{}", serde_json::to_string_pretty(&target)?);
        return Ok(());
    }
    print_layout(&inspected);
    Ok(())
}

pub fn plan(disk: &Path, target: &Path) -> anyhow::Result<()> {
    let inspected = inspect(disk)?;
    let plan = planned(&inspected.layout, target)?;
    print_plan(&inspected.layout, &plan);
    Ok(())
}

pub fn check(disk: &Path, target: &Path) -> anyhow::Result<()> {
    let inspected = inspect(disk)?;
    let plan = planned(&inspected.layout, target)?;
    let changed: Vec<_> = plan.operations.iter().map(Operation::partition).collect();

    let release = rustix::system::uname()
        .release()
        .to_string_lossy()
        .into_owned();
    let env = Environment {
        runner: &SystemRunner,
        sys: Path::new("/sys"),
        lib_modules: Path::new("/lib/modules"),
        boot: Path::new("/boot"),
        kernel_release: &release,
    };
    let facts = facts::gather(&env, &inspected.disk, &inspected.layout, &changed);
    let mut refusals = preflight::evaluate(&facts, &inspected.layout);
    refusals.extend(preflight::unavailable_operations(&inspected.layout, &plan));

    if refusals.is_empty() {
        println!("Every check passed.");
        return Ok(());
    }
    print_refusals(&refusals);
    bail!("{} check(s) failed", refusals.len())
}

fn inspect(disk: &Path) -> anyhow::Result<Inspected> {
    let all = disks(&SystemRunner).context("listing disks with lsblk")?;
    let info = find_disk(&all, disk)
        .with_context(|| format!("{} is not a disk lsblk knows about", disk.display()))?
        .clone();
    let device = RealDevice::open(disk, Access::ReadOnly).with_context(|| {
        format!(
            "opening {} (BootShift needs to run as root)",
            disk.display()
        )
    })?;
    let table = GptImage::read(&device).context("reading the partition table")?;
    let entries = table.entries()?;
    let layout = probe::layout(&SystemRunner, &info, table.usable(), &entries);
    Ok(Inspected { disk: info, layout })
}

fn planned(layout: &Layout, target: &Path) -> anyhow::Result<Plan> {
    let text =
        std::fs::read_to_string(target).with_context(|| format!("reading {}", target.display()))?;
    let target: Target = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a target layout", target.display()))?;
    match make_plan(layout, &target) {
        Ok(plan) => Ok(plan),
        Err(PlanError::Refused(refusal)) => {
            print_refusals(&[refusal]);
            bail!("the target layout was refused")
        }
        Err(PlanError::Internal(error)) => Err(error).context("planner bug"),
    }
}

fn print_layout(inspected: &Inspected) {
    let layout = &inspected.layout;
    let sector = layout.geometry.sector_size;
    println!(
        "{}: {}, {}-byte sectors",
        inspected.disk.path.display(),
        size(inspected.disk.size),
        sector
    );
    println!(
        "{:>3}  {:<20} {:>10} {:>10}  {:<13} {:>10} {:>14}",
        "#", "Name", "Start", "Size", "Contents", "Used", "Can shrink to"
    );
    for p in &layout.partitions {
        let min = p
            .min_len
            .map_or("-".to_string(), |s| size(s.to_bytes(sector)));
        println!(
            "{:>3}  {:<20} {:>10} {:>10}  {:<13} {:>10} {:>14}",
            p.number,
            truncate(&p.name, 20),
            size(p.extent.start.0 * u64::from(sector)),
            size(p.extent.len.to_bytes(sector)),
            p.content.name(),
            size(p.used.to_bytes(sector)),
            min
        );
    }
}

fn print_plan(layout: &Layout, plan: &Plan) {
    let sector = layout.geometry.sector_size;
    let name = |guid| {
        layout
            .partition(guid)
            .map_or_else(|| guid.to_string(), |p| p.label())
    };
    if plan.operations.is_empty() {
        println!("The disk already has this layout.");
        return;
    }
    for (index, operation) in plan.operations.iter().enumerate() {
        let step = match operation {
            Operation::Shrink { partition, len } => {
                format!(
                    "Shrink {} to {}",
                    name(*partition),
                    size(len.to_bytes(sector))
                )
            }
            Operation::Move {
                partition,
                to,
                copy_estimate,
                ..
            } => format!(
                "Move {} to {} (copies about {})",
                name(*partition),
                size(to.0 * u64::from(sector)),
                size(copy_estimate.to_bytes(sector))
            ),
            Operation::Grow { partition, len } => {
                format!(
                    "Grow {} to {}",
                    name(*partition),
                    size(len.to_bytes(sector))
                )
            }
            Operation::RemoveSwap { partition } => format!("Set aside swap {}", name(*partition)),
            Operation::RecreateSwap { partition } => format!(
                "Recreate swap {} at {}",
                partition.label(),
                size(partition.extent.start.0 * u64::from(sector))
            ),
        };
        println!("{:>2}. {step}", index + 1);
    }
    println!(
        "Copies about {} in total, during one restart.",
        size(plan.sectors_to_copy().to_bytes(sector))
    );
    let refused = preflight::unavailable_operations(layout, plan);
    if !refused.is_empty() {
        print_refusals(&refused);
    }
}

fn print_refusals(refusals: &[Refusal]) {
    for refusal in refusals {
        eprintln!("✗ {}\n  → {}", refusal.reason, refusal.fix);
    }
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(size(512), "512 B");
        assert_eq!(size(1536), "1.5 KiB");
        assert_eq!(size(256 << 20), "256.0 MiB");
        assert_eq!(size(512_110_190_592), "476.9 GiB");
    }
}
