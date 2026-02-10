//! File-based round-trip test: lay a SQLite DB → .egg → hatch back → compare.
//!
//! Usage: cargo run -p eggdb-io --example round_trip -- <source.db>

use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let source = Path::new(args.get(1).expect("usage: round_trip <source.db>"));
    let egg_out = std::env::temp_dir().join("roundtrip_test.egg");
    let hatch_out = std::env::temp_dir().join("roundtrip_hatched.db");

    // Clean up previous runs
    let _ = std::fs::remove_file(&egg_out);
    let _ = std::fs::remove_file(&hatch_out);

    let source_size = std::fs::metadata(source).expect("cannot stat source").len();
    println!(
        "Source: {} ({:.2} MB)",
        source.display(),
        source_size as f64 / 1_048_576.0
    );

    // --- LAY ---
    println!("\n=== LAY (SQLite -> .egg) ===");
    let start = Instant::now();
    let result =
        eggdb_io::writer::lay_file(source, &egg_out, &eggdb_io::writer::LayOptions::default())
            .expect("lay failed");
    let lay_time = start.elapsed();

    println!("Tables: {}", result.table_count);
    println!("Rows:   {}", result.total_rows);
    println!(
        "Original: {:.2} MB",
        result.original_size as f64 / 1_048_576.0
    );
    println!("Egg:      {:.2} MB", result.egg_size as f64 / 1_048_576.0);
    println!(
        "Ratio:    {:.1}x compression",
        result.original_size as f64 / result.egg_size as f64
    );
    println!("Time:     {:.2}s", lay_time.as_secs_f64());

    // --- VALIDATE ---
    println!("\n=== VALIDATE .egg ===");
    let egg_data = std::fs::read(&egg_out).expect("cannot read egg");
    let checks = eggdb_core::validate(&egg_data).expect("validate failed");
    for c in &checks {
        let status = if c.passed { "PASS" } else { "FAIL" };
        println!("  [{}] {} - {}", status, c.check, c.detail);
    }
    let all_passed = checks.iter().all(|c| c.passed);

    // --- HATCH ---
    println!("\n=== HATCH (.egg -> SQLite) ===");
    let start = Instant::now();
    let hatch_result = eggdb_io::reader::hatch_file(
        &egg_out,
        &hatch_out,
        &eggdb_io::reader::HatchOptions::default(),
    )
    .expect("hatch failed");
    let hatch_time = start.elapsed();

    let hatch_size = std::fs::metadata(&hatch_out)
        .expect("cannot stat hatch output")
        .len();
    println!("Tables: {}", hatch_result.table_count);
    println!("Rows:   {}", hatch_result.total_rows);
    println!("Output: {:.2} MB", hatch_size as f64 / 1_048_576.0);
    println!("Time:   {:.2}s", hatch_time.as_secs_f64());

    // --- COMPARE ---
    println!("\n=== ROW COUNT COMPARISON ===");
    let orig = rusqlite::Connection::open(source).expect("cannot open source");
    let hatched = rusqlite::Connection::open(&hatch_out).expect("cannot open hatched");

    let orig_tables = eggdb_io::sqlite_utils::get_table_names(&orig).expect("get tables");
    let hatch_tables = eggdb_io::sqlite_utils::get_table_names(&hatched).expect("get tables");

    if orig_tables != hatch_tables {
        eprintln!(
            "TABLE NAMES DIFFER: {:?} vs {:?}",
            orig_tables, hatch_tables
        );
        std::process::exit(1);
    }

    let mut all_match = true;
    for table in &orig_tables {
        let oc = eggdb_io::sqlite_utils::get_row_count(&orig, table).expect("count");
        let hc = eggdb_io::sqlite_utils::get_row_count(&hatched, table).expect("count");
        let tag = if oc == hc { "OK" } else { "MISMATCH" };
        if oc != hc {
            all_match = false;
        }
        println!("  [{}] {}: {} -> {}", tag, table, oc, hc);
    }

    // --- SPOT CHECK ---
    println!("\n=== SPOT CHECK (first 50 rows per table) ===");
    let mut total_mismatches = 0usize;
    for table in &orig_tables {
        let quoted = eggdb_io::sqlite_utils::quote_identifier(table);
        let sql = format!("SELECT * FROM {} LIMIT 50", quoted);

        let mut os = orig.prepare(&sql).expect("prepare");
        let mut hs = hatched.prepare(&sql).expect("prepare");
        let ncols = os.column_count();
        let mut or = os.query([]).expect("query");
        let mut hr = hs.query([]).expect("query");

        let mut table_mismatches = 0usize;
        let mut row_idx = 0u64;
        loop {
            match (or.next().expect("next"), hr.next().expect("next")) {
                (Some(o), Some(h)) => {
                    for c in 0..ncols {
                        let ov = format!("{:?}", o.get_ref(c).unwrap());
                        let hv = format!("{:?}", h.get_ref(c).unwrap());
                        if ov != hv {
                            if table_mismatches < 3 {
                                println!(
                                    "  {}.row[{}].col[{}]: {} vs {}",
                                    table,
                                    row_idx,
                                    c,
                                    &ov[..ov.len().min(60)],
                                    &hv[..hv.len().min(60)]
                                );
                            }
                            table_mismatches += 1;
                        }
                    }
                }
                (None, None) => break,
                _ => {
                    println!("  {}: row count mismatch at row {}", table, row_idx);
                    table_mismatches += 1;
                    break;
                }
            }
            row_idx += 1;
        }
        if table_mismatches == 0 {
            println!("  {}: OK ({} rows checked)", table, row_idx);
        } else {
            println!(
                "  {}: {} mismatches in {} rows",
                table, table_mismatches, row_idx
            );
        }
        total_mismatches += table_mismatches;
    }

    // --- SUMMARY ---
    println!("\n=== SUMMARY ===");
    println!(
        "Compression: {:.2} MB -> {:.2} MB ({:.1}x)",
        source_size as f64 / 1_048_576.0,
        result.egg_size as f64 / 1_048_576.0,
        source_size as f64 / result.egg_size as f64
    );
    println!("Lay:   {:.2}s", lay_time.as_secs_f64());
    println!("Hatch: {:.2}s", hatch_time.as_secs_f64());
    println!(
        "Validation: {}",
        if all_passed { "ALL PASS" } else { "FAILURES" }
    );
    println!(
        "Row counts: {}",
        if all_match { "ALL MATCH" } else { "MISMATCHES" }
    );
    println!(
        "Spot check: {}",
        if total_mismatches == 0 {
            "ALL MATCH".to_string()
        } else {
            format!("{} mismatches", total_mismatches)
        }
    );

    // Clean up
    let _ = std::fs::remove_file(&egg_out);
    let _ = std::fs::remove_file(&hatch_out);

    if !all_passed || !all_match || total_mismatches > 0 {
        std::process::exit(1);
    }
}
