//! Opt-in production-trust GPU pack probe for local diagnostics.
//!
//! The command authenticates the installed pack catalog and challenge-bound
//! worker Hello only. It accepts no paths, selectors, trust material, models,
//! audio, or timeout controls and never enters the inference/health registry.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};

const COMMAND_FLAG: &str = "--scribe-windows-gpu-pack-probe";
const SECURITY_RECORD_ACK: &str = "--ack-local-security-records";
const EXECUTION_COPY_ACK: &str = "--ack-local-execution-copies";
const MAX_REPORT_BYTES: usize = 64 * 1024;

pub(crate) fn maybe_run_local_command() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !args.iter().any(|arg| is_probe_argument(arg)) {
        return None;
    }
    Some(run_local_command(&args))
}

fn run_local_command(args: &[OsString]) -> i32 {
    let Some(allow_execution_copy) = execution_copy_acknowledgement(args) else {
        eprintln!(
            "GPU pack probing requires its command flag, local security-record acknowledgement and optional final execution-copy acknowledgement"
        );
        return 2;
    };

    let report = crate::onnx_worker::run_production_gpu_pack_probe(allow_execution_copy);
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

fn is_probe_argument(arg: &OsStr) -> bool {
    [COMMAND_FLAG, SECURITY_RECORD_ACK, EXECUTION_COPY_ACK]
        .iter()
        .any(|flag| arg == OsStr::new(flag))
}

fn execution_copy_acknowledgement(args: &[OsString]) -> Option<bool> {
    match args {
        [command, security]
            if command == OsStr::new(COMMAND_FLAG)
                && security == OsStr::new(SECURITY_RECORD_ACK) =>
        {
            Some(false)
        }
        [command, security, copies]
            if command == OsStr::new(COMMAND_FLAG)
                && security == OsStr::new(SECURITY_RECORD_ACK)
                && copies == OsStr::new(EXECUTION_COPY_ACK) =>
        {
            Some(true)
        }
        _ => None,
    }
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
            vec![OsString::from(EXECUTION_COPY_ACK)],
            vec![
                OsString::from(COMMAND_FLAG),
                OsString::from(EXECUTION_COPY_ACK),
            ],
            vec![
                OsString::from(SECURITY_RECORD_ACK),
                OsString::from(COMMAND_FLAG),
            ],
            vec![
                OsString::from(COMMAND_FLAG),
                OsString::from(SECURITY_RECORD_ACK),
                OsString::from("--output"),
            ],
            vec![
                OsString::from(COMMAND_FLAG),
                OsString::from(EXECUTION_COPY_ACK),
                OsString::from(SECURITY_RECORD_ACK),
            ],
            vec![
                OsString::from(COMMAND_FLAG),
                OsString::from(SECURITY_RECORD_ACK),
                OsString::from(EXECUTION_COPY_ACK),
                OsString::from(EXECUTION_COPY_ACK),
            ],
        ] {
            assert_eq!(run_local_command(&args), 2);
        }
    }

    #[test]
    fn execution_copy_ack_is_separate_optional_and_exact_without_starting_probe() {
        let arguments = [
            OsString::from(COMMAND_FLAG),
            OsString::from(SECURITY_RECORD_ACK),
        ];
        assert_eq!(execution_copy_acknowledgement(&arguments), Some(false));
        let mut arguments = arguments.to_vec();
        arguments.push(OsString::from(EXECUTION_COPY_ACK));
        assert_eq!(execution_copy_acknowledgement(&arguments), Some(true));
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
        if !args.iter().any(|arg| is_probe_argument(arg)) {
            None
        } else {
            Some(run_local_command(args))
        }
    }
}
