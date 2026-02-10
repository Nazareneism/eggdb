#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Feed arbitrary bytes to the .egg reader — must never panic.
    // Errors are expected and fine; panics are bugs.
    let _ = eggdb_core::EggReader::open(data);

    // Also try opening with a passphrase
    let _ = eggdb_core::EggReader::open_with_passphrase(data, "fuzz-pass");
});
