//! The read-only commands: probe, plan and check.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use bootshift_boot::initramfs::{self, Inputs};
use bootshift_core::layout::{Layout, Target};
use bootshift_core::plan::{Operation, Plan};
use bootshift_core::planner::{PlanError, plan as make_plan};
use bootshift_core::refusal::Refusal;
use bootshift_disk::{Access, GptImage, RealDevice};
use bootshift_engine::facts::{self, Environment};
use bootshift_engine::preflight::{self, Bootloader, Facts};
use bootshift_engine::prepare::{self, Prepare};
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
    let (_, refusals) = run_checks(&inspected, &plan);
    if refusals.is_empty() {
        println!("Every check passed.");
        return Ok(());
    }
    print_refusals(&refusals);
    bail!("{} check(s) failed", refusals.len())
}

pub fn apply(
    disk: &Path,
    target: &Path,
    confirmed: bool,
    init: Option<PathBuf>,
) -> anyhow::Result<()> {
    let inspected = inspect(disk)?;
    let plan = planned(&inspected.layout, target)?;
    if plan.operations.is_empty() {
        println!("The disk already has this layout.");
        return Ok(());
    }
    let (facts, refusals) = run_checks(&inspected, &plan);
    if !refusals.is_empty() {
        print_refusals(&refusals);
        bail!("{} check(s) failed, so nothing was changed", refusals.len());
    }
    print_plan(&inspected.layout, &plan);
    if !confirmed {
        println!(
            "Nothing has changed. Make sure your data is backed up, then run again with --yes."
        );
        return Ok(());
    }

    let init = match init {
        Some(path) => path,
        None => std::env::current_exe().context("finding the bootshift binary")?,
    };
    if !initramfs::shared_libraries(&SystemRunner, &init)?.is_empty() {
        bail!(
            "{} is not a static binary. Build one with `cargo build --release --target x86_64-unknown-linux-musl` and pass it with --init",
            init.display()
        );
    }
    let Bootloader::Grub { vendor } = &facts.bootloader else {
        bail!("no supported bootloader was found");
    };
    let esp_mount = facts::esp_mountpoint(&inspected.disk, &inspected.layout)
        .context("the EFI system partition isn't mounted")?;
    let release = kernel_release();
    let kernel_image = PathBuf::from(format!("/boot/vmlinuz-{release}"));
    let search_path = search_path();
    let work_dir = std::env::temp_dir().join(format!("bootshift-{}", std::process::id()));
    std::fs::create_dir_all(&work_dir).context("creating a work directory")?;

    prepare::prepare(&Prepare {
        runner: &SystemRunner,
        disk: &inspected.disk,
        layout: &inspected.layout,
        plan: &plan,
        esp_mount: &esp_mount,
        grub_vendor: vendor,
        kernel_image: &kernel_image,
        initramfs: Inputs {
            init: &init,
            kernel_release: &release,
            search_path: &search_path,
        },
        work_dir: &work_dir,
    })?;
    let _ = std::fs::remove_dir_all(&work_dir);

    println!("Everything is ready. Restart the computer to begin.");
    println!("If the power goes out during the change, just turn the computer back on.");
    Ok(())
}

fn run_checks(inspected: &Inspected, plan: &Plan) -> (Facts, Vec<Refusal>) {
    let changed: Vec<_> = plan.operations.iter().map(Operation::partition).collect();
    let release = kernel_release();
    let env = Environment {
        runner: &SystemRunner,
        sys: Path::new("/sys"),
        lib_modules: Path::new("/lib/modules"),
        boot: Path::new("/boot"),
        kernel_release: &release,
    };
    let facts = facts::gather(&env, &inspected.disk, &inspected.layout, &changed);
    let mut refusals = preflight::evaluate(&facts, &inspected.layout);
    refusals.extend(preflight::unavailable_operations(&inspected.layout, plan));
    (facts, refusals)
}

fn kernel_release() -> String {
    rustix::system::uname()
        .release()
        .to_string_lossy()
        .into_owned()
}

fn search_path() -> Vec<PathBuf> {
    [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
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
