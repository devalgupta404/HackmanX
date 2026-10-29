//! The operations the command line and the helper service share: inspect a disk, plan a
//! target, check it, and prepare it. `apply` runs every check again itself, so no caller can
//! skip them.

use std::path::{Path, PathBuf};

use bootshift_boot::cmdline::RecoveryArgs;
use bootshift_boot::initramfs::{self, Inputs};
use bootshift_core::layout::{Layout, Target};
use bootshift_core::plan::{ApplyError, Operation, Plan};
use bootshift_core::planner::{PlanError, plan};
use bootshift_core::refusal::Refusal;
use bootshift_disk::{Access, DiskError, GptError, GptImage, RealDevice};
use bootshift_fs::probe::{DiskInfo, disks, find_disk};
use bootshift_fs::run::{RunError, Runner};

use crate::executor;
use crate::facts::{self, Environment};
use crate::preflight::{self, Bootloader, Facts};
use crate::prepare::{self, Prepare, PrepareError};
use crate::probe;

/// Where the running system keeps the things the checks and the prepare step read.
pub struct SystemPaths {
    pub sys: PathBuf,
    pub lib_modules: PathBuf,
    pub boot: PathBuf,
    pub search_path: Vec<PathBuf>,
}

impl Default for SystemPaths {
    fn default() -> Self {
        Self {
            sys: PathBuf::from("/sys"),
            lib_modules: PathBuf::from("/lib/modules"),
            boot: PathBuf::from("/boot"),
            search_path: [
                "/usr/local/sbin",
                "/usr/local/bin",
                "/usr/sbin",
                "/usr/bin",
                "/sbin",
                "/bin",
            ]
            .iter()
            .map(PathBuf::from)
            .collect(),
        }
    }
}

pub struct Inspected {
    pub disk: DiskInfo,
    pub layout: Layout,
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{0} is not a disk the system knows about")]
    UnknownDisk(PathBuf),
    #[error("can't open {path}; BootShift needs to run as root")]
    Open {
        path: PathBuf,
        #[source]
        source: DiskError,
    },
    #[error(transparent)]
    Tool(#[from] RunError),
    #[error(transparent)]
    Table(#[from] GptError),
    #[error("{}", reasons(.0))]
    Refused(Vec<Refusal>),
    #[error("planner bug: {0}")]
    Planner(ApplyError),
    #[error(
        "{0} is not a static binary; build one with `cargo build --release --target x86_64-unknown-linux-musl`"
    )]
    NotStatic(PathBuf),
    #[error(transparent)]
    Prepare(#[from] PrepareError),
}

fn reasons(refusals: &[Refusal]) -> String {
    refusals
        .iter()
        .map(|r| r.reason.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn inspect(runner: &dyn Runner, disk: &Path) -> Result<Inspected, ServiceError> {
    let all = disks(runner)?;
    let info = find_disk(&all, disk)
        .ok_or_else(|| ServiceError::UnknownDisk(disk.to_path_buf()))?
        .clone();
    let device = RealDevice::open(disk, Access::ReadOnly).map_err(|source| ServiceError::Open {
        path: disk.to_path_buf(),
        source,
    })?;
    let table = GptImage::read(&device)?;
    let layout = probe::layout(runner, &info, table.usable(), &table.entries()?);
    Ok(Inspected { disk: info, layout })
}

/// The layout as it is now, as a target to edit.
pub fn current_target(layout: &Layout) -> Target {
    layout
        .partitions
        .iter()
        .map(|p| (p.unique_guid, p.extent))
        .collect()
}

pub fn plan_target(layout: &Layout, target: &Target) -> Result<Plan, ServiceError> {
    plan(layout, target).map_err(|error| match error {
        PlanError::Refused(refusal) => ServiceError::Refused(vec![refusal]),
        PlanError::Internal(error) => ServiceError::Planner(error),
    })
}

pub fn run_checks(
    runner: &dyn Runner,
    paths: &SystemPaths,
    inspected: &Inspected,
    plan: &Plan,
) -> (Facts, Vec<Refusal>) {
    let changed: Vec<_> = plan.operations.iter().map(Operation::partition).collect();
    let release = kernel_release();
    let env = Environment {
        runner,
        sys: &paths.sys,
        lib_modules: &paths.lib_modules,
        boot: &paths.boot,
        kernel_release: &release,
    };
    let facts = facts::gather(&env, &inspected.disk, &inspected.layout, &changed);
    let mut refusals = preflight::evaluate(&facts, &inspected.layout);
    refusals.extend(preflight::unavailable_operations(&inspected.layout, plan));
    (facts, refusals)
}

/// Checks everything again, then prepares the change for the next restart.
pub fn apply(
    runner: &dyn Runner,
    paths: &SystemPaths,
    inspected: &Inspected,
    plan: &Plan,
    init: &Path,
    work_dir: &Path,
) -> Result<RecoveryArgs, ServiceError> {
    let (facts, refusals) = run_checks(runner, paths, inspected, plan);
    if !refusals.is_empty() {
        return Err(ServiceError::Refused(refusals));
    }
    refuse_unworkable_plan(&inspected.disk.path, &inspected.layout, plan)?;
    if !initramfs::shared_libraries(runner, init)?.is_empty() {
        return Err(ServiceError::NotStatic(init.to_path_buf()));
    }
    let Bootloader::Grub { vendor } = &facts.bootloader else {
        return Err(refused("No supported bootloader was found.", "Use GRUB."));
    };
    let esp_mount = facts::esp_mountpoint(&inspected.disk, &inspected.layout).ok_or_else(|| {
        refused(
            "The EFI system partition isn't mounted.",
            "Mount it, usually at /boot/efi.",
        )
    })?;
    let release = kernel_release();
    let kernel_image = paths.boot.join(format!("vmlinuz-{release}"));

    Ok(prepare::prepare(&Prepare {
        runner,
        disk: &inspected.disk,
        layout: &inspected.layout,
        plan,
        esp_mount: &esp_mount,
        grub_vendor: vendor,
        kernel_image: &kernel_image,
        initramfs: Inputs {
            init,
            kernel_release: &release,
            search_path: &paths.search_path,
        },
        work_dir,
    })?)
}

/// Refuses a plan whose partition tables can't be built on this disk, before anything is
/// prepared, so the recovery system never meets it.
fn refuse_unworkable_plan(disk: &Path, layout: &Layout, plan: &Plan) -> Result<(), ServiceError> {
    let device = RealDevice::open(disk, Access::ReadOnly).map_err(|source| ServiceError::Open {
        path: disk.to_path_buf(),
        source,
    })?;
    executor::check_table_writes(&device, layout, plan).map_err(|error| {
        refused(
            &format!("BootShift can't carry out this change on this disk: {error}."),
            "Nothing was changed. Please report this as a BootShift bug.",
        )
    })
}

pub fn kernel_release() -> String {
    rustix::system::uname()
        .release()
        .to_string_lossy()
        .into_owned()
}

fn refused(reason: &str, fix: &str) -> ServiceError {
    ServiceError::Refused(vec![Refusal::new(reason, fix)])
}
