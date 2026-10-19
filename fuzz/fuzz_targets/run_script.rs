#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Enforce default limits with a strict wall-clock budget so execution
        // completes quickly and never hangs.
        let limits = flare::RuntimeLimits::default().with_max_wall_clock_ms(50);
        let _ = flare::run_script_with_services_and_limits(s, flare::Services::in_memory(), limits);
    }
});
