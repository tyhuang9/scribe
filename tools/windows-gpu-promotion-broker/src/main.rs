//! Unprivileged client for the separately privileged Windows GPU pack broker.
//!
//! This release binary intentionally has no signing key, ledger, filesystem
//! authority, fixture mode, or configurable broker endpoint. Until an
//! independently provisioned Windows service or remote HSM broker exists, it
//! validates the request shape in memory and fails closed.

use scribe_windows_gpu_promotion_broker::PromotionRequest;

fn main() {
    match PromotionRequest::parse_cli(std::env::args_os().skip(1)) {
        Ok(_) => {
            eprintln!(
                "Protected Windows GPU promotion broker is not provisioned; no filesystem, ledger, or signing authority was accessed."
            );
            std::process::exit(78);
        }
        Err(error) => {
            eprintln!("Protected Windows GPU promotion request rejected: {error}");
            std::process::exit(64);
        }
    }
}
