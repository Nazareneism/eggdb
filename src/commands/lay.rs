use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};

use crate::format::{format_duration, format_ratio, format_size};

#[allow(clippy::too_many_arguments)]
pub fn run(
    source: &Path,
    output: Option<&Path>,
    zstd_level: i32,
    adaptive_compression: bool,
    optimize_order: bool,
    streaming: bool,
    incremental: Option<&Path>,
    encrypt: bool,
) -> Result<()> {
    // Validate source exists
    anyhow::ensure!(
        source.exists(),
        "source file not found: {}",
        source.display()
    );

    // Determine output path
    let default_output = source.with_extension("egg");
    let output = output.unwrap_or(&default_output);

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg}")
            .unwrap()
            .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"]),
    );
    spinner.set_message(format!(
        "Laying {} ...",
        source.file_name().unwrap_or_default().to_string_lossy()
    ));
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    // Handle encryption: derive key from passphrase
    let (encryption_key, encryption_salt) = if encrypt {
        let passphrase = rpassword::prompt_password("Enter passphrase: ")
            .context("failed to read passphrase")?;
        anyhow::ensure!(!passphrase.is_empty(), "passphrase cannot be empty");

        let confirm = rpassword::prompt_password("Confirm passphrase: ")
            .context("failed to read passphrase confirmation")?;
        anyhow::ensure!(passphrase == confirm, "passphrases do not match");

        let salt = eggdb_core::generate_salt();
        let key = eggdb_core::derive_key(&passphrase, &salt)
            .context("failed to derive encryption key")?;
        (Some(key), Some(salt))
    } else {
        (None, None)
    };

    let options = eggdb_io::writer::LayOptions {
        zstd_level,
        row_order_preserved: !optimize_order,
        adaptive_compression,
        optimize_order,
        streaming,
        encryption_key,
        encryption_salt,
    };

    let start = Instant::now();

    let result = if let Some(existing_egg) = incremental {
        anyhow::ensure!(
            existing_egg.exists(),
            "existing egg file not found: {}",
            existing_egg.display()
        );
        eggdb_io::writer::incremental_lay_file(source, existing_egg, output, &options)
            .with_context(|| format!("failed incremental lay of {}", source.display()))?
    } else {
        eggdb_io::writer::lay_file(source, output, &options)
            .with_context(|| format!("failed to lay {}", source.display()))?
    };
    let elapsed = start.elapsed();

    spinner.finish_with_message("Done");

    println!();
    println!("  {} {}", "File:".bold(), output.display());
    println!("  {} {}", "Tables:".bold(), result.table_count);
    println!("  {} {}", "Rows:".bold(), result.total_rows);
    println!(
        "  {} {} → {}",
        "Size:".bold(),
        format_size(result.original_size),
        format_size(result.egg_size)
    );
    println!(
        "  {} {}",
        "Ratio:".bold(),
        format_ratio(result.original_size, result.egg_size)
    );
    println!("  {} {}", "Time:".bold(), format_duration(elapsed));

    if encrypt {
        println!("  {} encrypted", "🔒".bold());
    }

    Ok(())
}
