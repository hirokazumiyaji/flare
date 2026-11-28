#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Enforce default limits with a strict wall-clock budget so execution
        // completes quickly and never hangs.
        let limits = flare::RuntimeLimits::default().with_max_wall_clock_ms(50);
        let io = flare::InMemoryIo::new();
        // Fixed fixtures so File.read of known paths stays host-independent.
        io.insert("fixture.txt", "hello from fixture");
        io.insert("fixture.json", r#"{"status":"ok"}"#);
        io.insert("fixture.csv", "id,name\n1,alice\n");
        let services = flare::Services::in_memory().with_io(io);
        let _ = flare::run_script_with_services_and_limits(s, services, limits);
    }
});
