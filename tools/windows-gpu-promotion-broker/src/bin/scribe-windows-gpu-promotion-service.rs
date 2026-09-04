#[cfg(windows)]
fn main() {
    if scribe_windows_gpu_promotion_broker::harden_dll_search().is_err()
        || scribe_windows_gpu_promotion_broker::run_service_dispatcher().is_err()
    {
        std::process::exit(78);
    }
}

#[cfg(not(windows))]
fn main() {
    std::process::exit(78);
}
