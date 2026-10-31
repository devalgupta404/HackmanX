//! Disk health through smartmontools.

use std::path::Path;

use serde::Deserialize;

use crate::run::{CommandLine, Runner};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    Passed,
    Failing,
    /// The drive's health couldn't be read, so Kitten must treat it as unsafe.
    Unknown(String),
}

/// `smartctl` exit status bits that mean it couldn't talk to the drive at all: a command-line
/// error, or the device failed to open. Bit 2 ("a SMART command failed") is not fatal on its
/// own: drives that lack an optional log page set it while still reporting their health, QEMU's
/// NVMe among them. The health verdict in the JSON decides; no verdict stays `Unknown`.
const NO_DATA_BITS: i32 = 0b0000_0011;

/// The drive's own overall health assessment. A missing `smartctl` or an unreadable drive is
/// reported as `Unknown`, never as healthy.
pub fn health(runner: &dyn Runner, disk: &Path) -> Health {
    let command = CommandLine::new("smartctl")
        .arg("--json")
        .arg("--health")
        .path(disk);
    match runner.run(&command) {
        Err(error) => Health::Unknown(error.to_string()),
        Ok(output) => match output.status {
            Some(status) if status & NO_DATA_BITS == 0 => match parse_health(&output.text()) {
                Health::Unknown(detail) => {
                    Health::Unknown(format!("smartctl exited with {status}: {detail}"))
                }
                verdict => verdict,
            },
            status => Health::Unknown(format!("smartctl exited with {status:?}")),
        },
    }
}

pub fn parse_health(json: &str) -> Health {
    #[derive(Deserialize)]
    struct Report {
        smart_status: Option<Status>,
    }
    #[derive(Deserialize)]
    struct Status {
        passed: bool,
    }

    match serde_json::from_str::<Report>(json) {
        Ok(Report {
            smart_status: Some(Status { passed: true }),
        }) => Health::Passed,
        Ok(Report {
            smart_status: Some(Status { passed: false }),
        }) => Health::Failing,
        Ok(Report { smart_status: None }) => {
            Health::Unknown("the drive reported no health status".to_string())
        }
        Err(error) => Health::Unknown(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::ScriptedRunner;

    const PASSED: &str = r#"{"json_format_version":[1,0],"device":{"name":"/dev/nvme0n1","type":"nvme"},"smart_status":{"passed":true,"nvme":{"value":0}}}"#;
    const FAILED: &str = r#"{"device":{"name":"/dev/sda"},"smart_status":{"passed":false}}"#;

    #[test]
    fn reads_a_passing_and_a_failing_drive() {
        assert_eq!(parse_health(PASSED), Health::Passed);
        assert_eq!(parse_health(FAILED), Health::Failing);
    }

    #[test]
    fn missing_status_is_unknown() {
        assert!(matches!(
            parse_health(r#"{"device":{}}"#),
            Health::Unknown(_)
        ));
    }

    #[test]
    fn a_failing_drive_still_parses_despite_its_exit_status() {
        let runner = ScriptedRunner::new().reply(8, FAILED);
        assert_eq!(health(&runner, Path::new("/dev/sda")), Health::Failing);
    }

    #[test]
    fn a_verdict_counts_even_when_one_smart_command_failed() {
        // QEMU's NVMe: an optional log page is unsupported (exit bit 2), the verdict is real.
        let runner = ScriptedRunner::new().reply(4, PASSED);
        assert_eq!(health(&runner, Path::new("/dev/nvme0n1")), Health::Passed);
    }

    #[test]
    fn an_exit_bit_2_without_a_verdict_stays_unknown() {
        let runner = ScriptedRunner::new().reply(4, r#"{"device":{}}"#);
        assert!(matches!(
            health(&runner, Path::new("/dev/nvme0n1")),
            Health::Unknown(_)
        ));
    }

    #[test]
    fn smartctl_that_cannot_open_the_drive_is_unknown() {
        let runner = ScriptedRunner::new().reply_error(2, "Smartctl open device failed");
        assert!(matches!(
            health(&runner, Path::new("/dev/sda")),
            Health::Unknown(_)
        ));
    }

    #[test]
    fn missing_smartctl_is_unknown() {
        assert!(matches!(
            health(&ScriptedRunner::new(), Path::new("/dev/sda")),
            Health::Unknown(_)
        ));
    }
}
