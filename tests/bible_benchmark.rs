//! End-to-end benchmark tests against `tests/fixtures/bible.eng.db`.
//!
//! These tests verify that compression ratios and timings don't regress beyond
//! documented thresholds, and report actual numbers vs documented baselines so
//! improvements are visible too.
//!
//! All tests are `#[ignore]` because they're too slow for normal `cargo test`
//! (the zstd-19 test alone takes ~7 minutes).
//!
//! Run explicitly:
//! ```bash
//! cargo test --release --test bible_benchmark -- --ignored --nocapture
//! ```
//!
//! Tests skip gracefully if the fixture is missing.

use std::path::Path;
use std::time::Instant;

use eggdb_core::{validate, EggReader};
use eggdb_io::reader::{hatch_file, HatchOptions};
use eggdb_io::writer::{lay_file, LayOptions};

const BIBLE_DB: &str = "tests/fixtures/bible.eng.db";
const BIBLE_EGG: &str = "tests/fixtures/bible.eng.egg";
const EXPECTED_TABLES: usize = 11;
const EXPECTED_ROWS: u64 = 1_497_017;

// Documented baselines from README (Apple Silicon, release build).
mod baseline {
    // Compression ratios
    pub const DEFAULT_RATIO: f64 = 5.1;
    pub const ADAPTIVE_RATIO: f64 = 5.9;
    pub const ZSTD19_RATIO: f64 = 8.5;
    pub const ZSTD19_ADAPTIVE_RATIO: f64 = 8.5;

    // Timings (seconds)
    pub const DEFAULT_LAY_TIME: f64 = 12.2;
    pub const ADAPTIVE_LAY_TIME: f64 = 21.7;
    pub const HATCH_TIME: f64 = 6.9;
    pub const VALIDATE_TIME: f64 = 0.09;

    // Regression floors (fail below these)
    pub const DEFAULT_RATIO_FLOOR: f64 = 4.5;
    pub const ADAPTIVE_RATIO_FLOOR: f64 = 5.0;
    pub const ZSTD19_RATIO_FLOOR: f64 = 8.0;
    pub const ZSTD19_ADAPTIVE_RATIO_FLOOR: f64 = 8.0;
    pub const DEFAULT_LAY_TIME_CEIL: u64 = 40;
    pub const HATCH_TIME_CEIL: u64 = 20;
    pub const VALIDATE_TIME_CEIL: u64 = 5;
}

fn skip_if_no_fixture() -> bool {
    if !Path::new(BIBLE_DB).exists() {
        eprintln!("SKIP: {} not found", BIBLE_DB);
        return true;
    }
    false
}

fn delta_str(actual: f64, documented: f64) -> String {
    let pct = (actual - documented) / documented * 100.0;
    if pct >= 0.0 {
        format!("+{:.1}%", pct)
    } else {
        format!("{:.1}%", pct)
    }
}

fn report_ratio(label: &str, actual: f64, documented: f64) {
    eprintln!(
        "  ratio:  {:.2}x  (documented: {:.1}x, {})",
        actual,
        documented,
        delta_str(actual, documented)
    );
    if actual > documented {
        eprintln!("  ^ IMPROVED over documented baseline for {}", label);
    } else if actual < documented {
        eprintln!("  ^ regressed from documented baseline for {}", label);
    }
}

fn report_time(label: &str, actual: f64, documented: f64) {
    // For time, lower is better — flip the sign for the delta display.
    let pct = (actual - documented) / documented * 100.0;
    let qualifier = if pct <= -1.0 {
        "faster"
    } else if pct >= 1.0 {
        "slower"
    } else {
        "same"
    };
    eprintln!(
        "  time:   {:.2}s  (documented: {:.1}s, {}, {})",
        actual,
        documented,
        delta_str(actual, documented),
        qualifier
    );
    if actual < documented * 0.95 {
        eprintln!("  ^ IMPROVED over documented baseline for {}", label);
    } else if actual > documented * 1.05 {
        eprintln!("  ^ regressed from documented baseline for {}", label);
    }
}

fn report_lay(
    label: &str,
    result: &eggdb_io::writer::LayResult,
    elapsed: f64,
    doc_ratio: f64,
    doc_time: f64,
) {
    eprintln!("\n--- {} ---", label);
    eprintln!(
        "  size:   {} -> {}",
        format_size(result.original_size),
        format_size(result.egg_size)
    );
    let ratio = result.original_size as f64 / result.egg_size as f64;
    report_ratio(label, ratio, doc_ratio);
    report_time(label, elapsed, doc_time);
}

#[test]
#[ignore]
fn lay_default() {
    if skip_if_no_fixture() {
        return;
    }

    let output = tempfile::NamedTempFile::new().unwrap();
    let options = LayOptions::default();

    let start = Instant::now();
    let result = lay_file(Path::new(BIBLE_DB), output.path(), &options).unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    let ratio = result.original_size as f64 / result.egg_size as f64;

    report_lay(
        "lay_default",
        &result,
        elapsed,
        baseline::DEFAULT_RATIO,
        baseline::DEFAULT_LAY_TIME,
    );

    assert_eq!(result.table_count, EXPECTED_TABLES);
    assert_eq!(result.total_rows, EXPECTED_ROWS);
    assert!(
        ratio >= baseline::DEFAULT_RATIO_FLOOR,
        "compression ratio {:.2}x below {:.1}x threshold",
        ratio,
        baseline::DEFAULT_RATIO_FLOOR
    );
    assert!(
        elapsed as u64 <= baseline::DEFAULT_LAY_TIME_CEIL,
        "lay took {:.1}s, exceeds {}s threshold",
        elapsed,
        baseline::DEFAULT_LAY_TIME_CEIL
    );
}

#[test]
#[ignore]
fn lay_adaptive() {
    if skip_if_no_fixture() {
        return;
    }

    let output = tempfile::NamedTempFile::new().unwrap();
    let options = LayOptions {
        adaptive_compression: true,
        ..Default::default()
    };

    let start = Instant::now();
    let result = lay_file(Path::new(BIBLE_DB), output.path(), &options).unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    let ratio = result.original_size as f64 / result.egg_size as f64;

    report_lay(
        "lay_adaptive",
        &result,
        elapsed,
        baseline::ADAPTIVE_RATIO,
        baseline::ADAPTIVE_LAY_TIME,
    );

    assert_eq!(result.table_count, EXPECTED_TABLES);
    assert!(
        ratio >= baseline::ADAPTIVE_RATIO_FLOOR,
        "adaptive ratio {:.2}x below {:.1}x threshold",
        ratio,
        baseline::ADAPTIVE_RATIO_FLOOR
    );
}

#[test]
#[ignore]
fn lay_zstd19() {
    if skip_if_no_fixture() {
        return;
    }

    // This test writes to the fixture path for use by other tools.
    let options = LayOptions {
        zstd_level: 19,
        ..Default::default()
    };

    let start = Instant::now();
    let result = lay_file(Path::new(BIBLE_DB), Path::new(BIBLE_EGG), &options).unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    let ratio = result.original_size as f64 / result.egg_size as f64;

    report_lay(
        "lay_zstd19",
        &result,
        elapsed,
        baseline::ZSTD19_RATIO,
        430.0,
    );

    assert_eq!(result.table_count, EXPECTED_TABLES);
    assert!(
        ratio >= baseline::ZSTD19_RATIO_FLOOR,
        "zstd-19 ratio {:.2}x below {:.1}x threshold",
        ratio,
        baseline::ZSTD19_RATIO_FLOOR
    );
}

#[test]
#[ignore]
fn lay_zstd19_adaptive() {
    if skip_if_no_fixture() {
        return;
    }

    let output = tempfile::NamedTempFile::new().unwrap();
    let options = LayOptions {
        zstd_level: 19,
        adaptive_compression: true,
        ..Default::default()
    };

    let start = Instant::now();
    let result = lay_file(Path::new(BIBLE_DB), output.path(), &options).unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    let ratio = result.original_size as f64 / result.egg_size as f64;

    report_lay(
        "lay_zstd19_adaptive",
        &result,
        elapsed,
        baseline::ZSTD19_ADAPTIVE_RATIO,
        340.0,
    );

    assert_eq!(result.table_count, EXPECTED_TABLES);
    assert!(
        ratio >= baseline::ZSTD19_ADAPTIVE_RATIO_FLOOR,
        "zstd-19+adaptive ratio {:.2}x below {:.1}x threshold",
        ratio,
        baseline::ZSTD19_ADAPTIVE_RATIO_FLOOR
    );
}

#[test]
#[ignore]
fn hatch_round_trip() {
    if skip_if_no_fixture() {
        return;
    }

    // First lay a default .egg to a tempfile.
    let egg_file = tempfile::NamedTempFile::new().unwrap();
    let lay_options = LayOptions::default();
    lay_file(Path::new(BIBLE_DB), egg_file.path(), &lay_options).unwrap();

    // Hatch it back to SQLite.
    let sqlite_file = tempfile::NamedTempFile::new().unwrap();
    let hatch_options = HatchOptions::default();

    let start = Instant::now();
    let result = hatch_file(egg_file.path(), sqlite_file.path(), &hatch_options).unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    eprintln!("\n--- hatch_round_trip ---");
    eprintln!(
        "  tables: {}  rows: {}",
        result.table_count, result.total_rows
    );
    report_time("hatch_round_trip", elapsed, baseline::HATCH_TIME);

    assert_eq!(result.table_count, EXPECTED_TABLES);
    assert_eq!(result.total_rows, EXPECTED_ROWS);
    assert!(
        elapsed as u64 <= baseline::HATCH_TIME_CEIL,
        "hatch took {:.1}s, exceeds {}s threshold",
        elapsed,
        baseline::HATCH_TIME_CEIL
    );
}

#[test]
#[ignore]
fn validate_checksums() {
    if skip_if_no_fixture() {
        return;
    }

    // Lay a default .egg to validate.
    let egg_file = tempfile::NamedTempFile::new().unwrap();
    let lay_options = LayOptions::default();
    lay_file(Path::new(BIBLE_DB), egg_file.path(), &lay_options).unwrap();

    let data = std::fs::read(egg_file.path()).unwrap();

    // Verify we can open and read metadata.
    let reader = EggReader::open(&data).unwrap();
    assert_eq!(reader.tables().len(), EXPECTED_TABLES);

    // Run validation.
    let start = Instant::now();
    let results = validate(&data).unwrap();
    let elapsed = start.elapsed().as_secs_f64();

    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed: Vec<_> = results.iter().filter(|r| !r.passed).collect();

    eprintln!("\n--- validate_checksums ---");
    eprintln!("  checks: {}/{} passed", passed, total);
    report_time("validate_checksums", elapsed, baseline::VALIDATE_TIME);

    assert!(
        failed.is_empty(),
        "validation failures: {:?}",
        failed
            .iter()
            .map(|r| format!("{}: {}", r.check, r.detail))
            .collect::<Vec<_>>()
    );
    assert!(
        elapsed as u64 <= baseline::VALIDATE_TIME_CEIL,
        "validate took {:.1}s, exceeds {}s threshold",
        elapsed,
        baseline::VALIDATE_TIME_CEIL
    );
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
