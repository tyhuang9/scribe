//! Unprivileged client for the separately privileged Windows GPU pack broker.
//!
//! This release binary intentionally has no signing key, ledger, filesystem
//! authority, fixture mode, or configurable broker endpoint. On Windows it
//! contacts only the fixed, authenticated no-authority service endpoint. The
//! service can return only a typed `NotProvisioned` response.

use scribe_windows_gpu_promotion_broker::ClientInvocation;

fn main() {
    #[cfg(windows)]
    if scribe_windows_gpu_promotion_broker::harden_dll_search().is_err() {
        eprintln!("Protected Windows GPU promotion client initialization failed.");
        std::process::exit(74);
    }

    let invocation = match ClientInvocation::parse_cli(std::env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(_) => {
            eprintln!("Protected Windows GPU promotion intent was rejected.");
            std::process::exit(64);
        }
    };

    #[cfg(windows)]
    match scribe_windows_gpu_promotion_broker::request_promotion(&invocation.intent) {
        Ok(_) | Err(scribe_windows_gpu_promotion_broker::ClientTransportError::Unavailable) => {
            eprintln!(
                "Protected Windows GPU promotion broker is not provisioned; no filesystem, ledger, or signing authority was accessed."
            );
            std::process::exit(78);
        }
        Err(_) => {
            eprintln!("Protected Windows GPU promotion transport was rejected.");
            std::process::exit(74);
        }
    }

    #[cfg(not(windows))]
    {
        let _ = invocation;
        eprintln!(
            "Protected Windows GPU promotion broker is not provisioned; no filesystem, ledger, or signing authority was accessed."
        );
        std::process::exit(78);
    }
}
