//! `status`: read the receiver's journal slot off the scratch device and report how far the
//! migration has got (the newest-wins durable record, implementation I4).

use std::path::Path;

use anyhow::{Context, Result};
use bootshift_disk::Access;
use bootshift_migrate_receive::Store;

use crate::common::open_device;

pub fn run(scratch: &Path, block_size: u32) -> Result<()> {
    let mut device = open_device(scratch, Access::ReadOnly, block_size)?;
    let store = Store::new(&mut device);
    match store
        .read_newest()
        .context("reading the newest journal slot")?
    {
        Some(slot) => {
            println!("receiver state (from {}):", scratch.display());
            println!("  run id:         {}", slot.run_id);
            println!("  epoch:          {}", slot.epoch.0);
            println!("  acknowledged:   seq {}", slot.acked_seq.0);
            println!("  journal slot:   {}", slot.seq);
        }
        None => println!("no migration journal found on {}", scratch.display()),
    }
    Ok(())
}
