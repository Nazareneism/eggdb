#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Feed arbitrary bytes as a string to the YQL parser — must never panic.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = eggdb_yql::parse(s);
        let _ = eggdb_yql::transpile(s);
        let _ = eggdb_yql::is_yql(s);
    }
});
