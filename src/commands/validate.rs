use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

pub fn run(egg: &Path, passphrase: Option<&str>) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());

    let data = std::fs::read(egg).with_context(|| format!("failed to read {}", egg.display()))?;

    let encryption_key = if let Some(pass) = passphrase {
        // Read the salt from the yolk section to derive the key
        let reader = eggdb_core::EggReader::open(&data)
            .with_context(|| format!("failed to parse {}", egg.display()))?;
        let header = reader.header();
        if header.flags.has_encryption() {
            let salt_start = header.yolk_offset as usize;
            let salt_end = salt_start + eggdb_core::SALT_SIZE;
            anyhow::ensure!(
                salt_end <= data.len(),
                "egg file too short for encryption salt"
            );
            let salt: [u8; 16] = data[salt_start..salt_end].try_into().unwrap();
            let key =
                eggdb_core::derive_key(pass, &salt).context("failed to derive encryption key")?;
            Some(key)
        } else {
            eprintln!(
                "{}: file is not encrypted, ignoring --passphrase",
                "Warning".yellow()
            );
            None
        }
    } else {
        None
    };

    let results = eggdb_core::validate_with_key(&data, encryption_key.as_deref())
        .with_context(|| format!("failed to validate {}", egg.display()))?;

    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = total - passed;

    for r in &results {
        let status = if r.passed {
            "PASS".green().bold()
        } else {
            "FAIL".red().bold()
        };
        println!("  [{}] {} — {}", status, r.check, r.detail);
    }

    println!();
    if failed == 0 {
        println!(
            "  {} {}/{} checks passed",
            "✓".green().bold(),
            passed,
            total
        );
    } else {
        println!(
            "  {} {}/{} checks passed, {} failed",
            "✗".red().bold(),
            passed,
            total,
            failed
        );
    }

    if failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}
