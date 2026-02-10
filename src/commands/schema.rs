use std::path::Path;

use anyhow::{Context, Result};

pub fn run(egg: &Path) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());

    let data = std::fs::read(egg).with_context(|| format!("failed to read {}", egg.display()))?;

    let reader = eggdb_core::EggReader::open(&data)
        .with_context(|| format!("failed to parse {}", egg.display()))?;

    let tables = reader.tables();

    for (i, table) in tables.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}", table.original_ddl);
        for index_ddl in &table.index_definitions {
            println!("{index_ddl}");
        }
    }

    Ok(())
}
