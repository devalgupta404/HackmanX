//! The firmware's boot order, through `efibootmgr`.
//!
//! A one-time `BootNext` is consumed on the first boot, so after a power cut the firmware would
//! go back to its normal order. Instead BootShift sets a persistent order with the running
//! distro's entry first and Windows Boot Manager taken out, and restores the original at the
//! end. The Windows entry itself is kept.

use bootshift_fs::run::{CommandLine, RunError, Runner};

const WINDOWS_LABEL: &str = "Windows Boot Manager";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootEntries {
    /// The entry the running system was started from.
    pub current: Option<u16>,
    pub order: Vec<u16>,
    pub entries: Vec<BootEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootEntry {
    pub number: u16,
    pub label: String,
}

pub fn read(runner: &dyn Runner) -> Result<BootEntries, RunError> {
    let command = CommandLine::new("efibootmgr");
    let output = runner.run_ok(&command)?;
    parse(&output.text()).map_err(|detail| RunError::UnexpectedOutput {
        command: command.to_string(),
        detail,
    })
}

pub fn set_order(runner: &dyn Runner, order: &[u16]) -> Result<(), RunError> {
    let order: Vec<String> = order.iter().map(|n| format!("{n:04X}")).collect();
    let command = CommandLine::new("efibootmgr")
        .arg("--bootorder")
        .arg(order.join(","));
    runner.run_ok(&command).map(|_| ())
}

/// The order to use while BootShift works: the running system's entry first, everything else
/// after it, except Windows Boot Manager.
pub fn recovery_order(entries: &BootEntries) -> Result<Vec<u16>, String> {
    let current = entries
        .current
        .ok_or("the firmware doesn't say which entry started this system")?;
    let is_windows = |number: &u16| {
        entries
            .entries
            .iter()
            .any(|e| e.number == *number && e.label.contains(WINDOWS_LABEL))
    };
    let mut order = vec![current];
    order.extend(
        entries
            .order
            .iter()
            .filter(|n| **n != current && !is_windows(n))
            .copied(),
    );
    Ok(order)
}

pub fn parse(text: &str) -> Result<BootEntries, String> {
    let mut current = None;
    let mut order = Vec::new();
    let mut entries = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("BootCurrent:") {
            current = Some(parse_hex_u16(value.trim())?);
        } else if let Some(value) = line.strip_prefix("BootOrder:") {
            order = value
                .trim()
                .split(',')
                .filter(|n| !n.is_empty())
                .map(parse_hex_u16)
                .collect::<Result<_, _>>()?;
        } else if let Some(rest) = line.strip_prefix("Boot") {
            let Some(number) = rest.get(..4).and_then(|n| parse_hex_u16(n).ok()) else {
                continue;
            };
            let label = rest
                .get(4..)
                .unwrap_or_default()
                .trim_start_matches('*')
                .split('\t')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            entries.push(BootEntry { number, label });
        }
    }
    Ok(BootEntries {
        current,
        order,
        entries,
    })
}

fn parse_hex_u16(text: &str) -> Result<u16, String> {
    u16::from_str_radix(text, 16).map_err(|_| format!("\"{text}\" is not a boot entry number"))
}

#[cfg(test)]
mod tests {
    use bootshift_fs::run::ScriptedRunner;

    use super::*;

    const DUAL_BOOT: &str = "BootCurrent: 0001\n\
        Timeout: 0 seconds\n\
        BootOrder: 0000,0001,2001,2002\n\
        Boot0000* Windows Boot Manager\tHD(1,GPT,0b5e2f6c,0x800,0x82000)/File(\\EFI\\Microsoft\\Boot\\bootmgfw.efi)\n\
        Boot0001* ubuntu\tHD(1,GPT,0b5e2f6c,0x800,0x82000)/File(\\EFI\\ubuntu\\shimx64.efi)\n\
        Boot2001* EFI USB Device\tRC\n\
        Boot2002* EFI Network\tRC\n";

    #[test]
    fn parses_entries_order_and_the_current_entry() {
        let entries = parse(DUAL_BOOT).unwrap();
        assert_eq!(entries.current, Some(1));
        assert_eq!(entries.order, [0x0000, 0x0001, 0x2001, 0x2002]);
        assert_eq!(entries.entries[0].label, "Windows Boot Manager");
        assert_eq!(entries.entries[1].label, "ubuntu");
    }

    #[test]
    fn recovery_order_puts_linux_first_and_leaves_windows_out() {
        let order = recovery_order(&parse(DUAL_BOOT).unwrap()).unwrap();
        assert_eq!(order, [0x0001, 0x2001, 0x2002]);
    }

    #[test]
    fn recovery_order_needs_to_know_the_current_entry() {
        let without_current = DUAL_BOOT.replace("BootCurrent: 0001\n", "");
        assert!(recovery_order(&parse(&without_current).unwrap()).is_err());
    }

    #[test]
    fn sets_the_order_in_hex() {
        let runner = ScriptedRunner::new().reply(0, "");
        set_order(&runner, &[0x0001, 0x2001]).unwrap();
        assert_eq!(runner.commands(), ["efibootmgr --bootorder 0001,2001"]);
    }
}
