//! Opt-in production-trust GPU pack probe for local diagnostics.
//!
//! The command authenticates the installed pack catalog and challenge-bound
//! worker Hello only. It accepts no paths, selectors, trust material, models,
//! audio, or timeout controls and never enters the inference/health registry.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};

const COMMAND_FLAG: &str = "--scribe-windows-gpu-pack-probe";
const SECURITY_RECORD_ACK: &str = "--ack-local-security-records";
const MAX_REPORT_BYTES: usize = 64 * 1024;

pub(crate) fn maybe_run_local_command() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !args
        .iter()
        .any(|arg| arg == OsStr::new(COMMAND_FLAG) || arg == OsStr::new(SECURITY_RECORD_ACK))
    {
        return None;
    }
    Some(run_local_command(&args))
}

fn run_local_command(args: &[OsString]) -> i32 {
    let expected = [OsStr::new(COMMAND_FLAG), OsStr::new(SECURITY_RECORD_ACK)];
    if args.len() != expected.len()
        || args
            .iter()
            .zip(expected)
            .any(|(actual, expected)| actual != expected)
    {
        eprintln!(
            "GPU pack probing requires exactly its command flag and local security-record acknowledgement"
        );
        return 2;
    }

    let report = crate::onnx_worker::run_production_gpu_pack_probe();
    let successful = probe_exit_code(
        report.probe_completed,
        report.cleanup_confirmed,
        report.authenticated_devices.len(),
    ) == 0;
    let mut encoded = match serde_json::to_vec(&report) {
        Ok(encoded) if encoded.len() < MAX_REPORT_BYTES => encoded,
        Ok(_) | Err(_) => {
            eprintln!("GPU pack probe report could not be encoded within its fixed bound");
            return 1;
        }
    };
    encoded.push(b'\n');
    if io::stdout().lock().write_all(&encoded).is_err() {
        return 1;
    }
    if successful { 0 } else { 1 }
}

fn probe_exit_code(probe_completed: bool, cleanup_confirmed: bool, device_count: usize) -> i32 {
    if probe_completed && cleanup_confirmed && device_count != 0 {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_arguments_require_the_explicit_security_record_acknowledgement() {
        for args in [
            vec![OsString::from(COMMAND_FLAG)],
            vec![OsString::from(SECURITY_RECORD_ACK)],
            vec![
                OsString::from(SECURITY_RECORD_ACK),
                OsString::from(COMMAND_FLAG),
            ],
            vec![
                OsString::from(COMMAND_FLAG),
                OsString::from(SECURITY_RECORD_ACK),
                OsString::from("--output"),
            ],
        ] {
            assert_eq!(run_local_command(&args), 2);
        }
    }

    #[test]
    fn unrelated_arguments_do_not_enter_probe_mode() {
        assert!(maybe_run_local_command_for_test(&[OsString::from("--unrelated")]).is_none());
    }

    #[test]
    fn incomplete_empty_or_unconfirmed_probe_never_returns_success() {
        assert_eq!(probe_exit_code(true, true, 1), 0);
        assert_eq!(probe_exit_code(false, true, 1), 1);
        assert_eq!(probe_exit_code(true, false, 1), 1);
        assert_eq!(probe_exit_code(true, true, 0), 1);
    }

    fn maybe_run_local_command_for_test(args: &[OsString]) -> Option<i32> {
        if !args
            .iter()
            .any(|arg| arg == OsStr::new(COMMAND_FLAG) || arg == OsStr::new(SECURITY_RECORD_ACK))
        {
            None
        } else {
            Some(run_local_command(args))
        }
    }
}
