mod commands;
mod format;

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "egg", about = "A lifecycle-aware compressed database engine")]
#[command(version, propagate_version = true)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compress a SQLite database into an .egg file
    Lay {
        /// Path to the source SQLite database
        source: PathBuf,

        /// Output .egg file path (default: <source>.egg)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Zstd compression level (1-22, default: 3)
        #[arg(long, default_value = "3")]
        zstd_level: i32,

        /// Automatically select per-column zstd levels based on encoding type
        #[arg(long)]
        adaptive_compression: bool,

        /// Sort rows by lowest-cardinality column to improve compression
        #[arg(long)]
        optimize_order: bool,

        /// Process tables in streaming mode (column-at-a-time, lower peak memory)
        #[arg(long)]
        streaming: bool,

        /// Re-encode only changed columns from an existing .egg file
        #[arg(long)]
        incremental: Option<PathBuf>,

        /// Encrypt yolk data blocks with AES-256-GCM (prompts for passphrase)
        #[arg(long)]
        encrypt: bool,
    },

    /// Restore an .egg file back to a SQLite database
    Hatch {
        /// Path to the .egg file
        egg: PathBuf,

        /// Output SQLite file path (default: <egg>.sqlite)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Overwrite output file if it exists
        #[arg(long)]
        force: bool,

        /// Only restore specified tables (comma-separated)
        #[arg(long, value_delimiter = ',')]
        tables: Option<Vec<String>>,

        /// Passphrase for encrypted .egg files
        #[arg(long)]
        passphrase: Option<String>,
    },

    /// Quick metadata summary of an .egg file
    Candle {
        /// Path to the .egg file
        egg: PathBuf,
    },

    /// Per-column compression statistics
    Info {
        /// Path to the .egg file
        egg: PathBuf,

        /// Show detailed info for a specific table
        #[arg(long)]
        table: Option<String>,
    },

    /// Verify all checksums in an .egg file
    Validate {
        /// Path to the .egg file
        egg: PathBuf,

        /// Passphrase for encrypted .egg files
        #[arg(long)]
        passphrase: Option<String>,
    },

    /// Print the original DDL from an .egg file
    Schema {
        /// Path to the .egg file
        egg: PathBuf,
    },

    /// Export data to CSV or JSON without rebuilding SQLite
    Crack {
        /// Path to the .egg file
        egg: PathBuf,

        /// Output format: csv or json
        #[arg(long, default_value = "csv")]
        format: String,

        /// Export only a specific table
        #[arg(long)]
        table: Option<String>,

        /// Passphrase for encrypted .egg files
        #[arg(long)]
        passphrase: Option<String>,
    },

    /// Query an .egg file using YQL or SQL
    Query {
        /// Path to the .egg file
        egg: PathBuf,

        /// YQL or SQL query string
        query: String,

        /// Passphrase for encrypted .egg files
        #[arg(long)]
        passphrase: Option<String>,
    },

    /// Interactive YQL/SQL shell for an .egg file
    Shell {
        /// Path to the .egg file
        egg: PathBuf,

        /// Passphrase for encrypted .egg files
        #[arg(long)]
        passphrase: Option<String>,
    },

    /// Compare two .egg files (schema, encodings, sizes)
    Diff {
        /// Path to the first .egg file
        a: PathBuf,

        /// Path to the second .egg file
        b: PathBuf,
    },

    /// Re-analyze and re-encode an .egg file without round-tripping through SQLite
    Relay {
        /// Path to the source .egg file
        egg: PathBuf,

        /// Output .egg file path (default: <source>_relayed.egg)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Zstd compression level (1-22, default: 3)
        #[arg(long, default_value = "3")]
        zstd_level: i32,

        /// Passphrase for encrypted .egg files
        #[arg(long)]
        passphrase: Option<String>,
    },

    /// Browse an .egg file in a local web viewer
    Web {
        /// Path to the .egg file
        egg: PathBuf,

        /// Port for the HTTP server (default: 8080)
        #[arg(long, default_value = "8080")]
        port: u16,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Lay {
            source,
            output,
            zstd_level,
            adaptive_compression,
            optimize_order,
            streaming,
            incremental,
            encrypt,
        } => commands::lay::run(
            &source,
            output.as_deref(),
            zstd_level,
            adaptive_compression,
            optimize_order,
            streaming,
            incremental.as_deref(),
            encrypt,
        ),

        Commands::Hatch {
            egg,
            output,
            force,
            tables,
            passphrase,
        } => commands::hatch::run(
            &egg,
            output.as_deref(),
            force,
            tables,
            passphrase.as_deref(),
        ),

        Commands::Candle { egg } => commands::candle::run(&egg),

        Commands::Info { egg, table } => commands::info::run(&egg, table.as_deref()),

        Commands::Validate { egg, passphrase } => {
            commands::validate::run(&egg, passphrase.as_deref())
        }

        Commands::Schema { egg } => commands::schema::run(&egg),

        Commands::Crack {
            egg,
            format,
            table,
            passphrase,
        } => commands::crack::run(&egg, &format, table.as_deref(), passphrase.as_deref()),

        Commands::Query {
            egg,
            query,
            passphrase,
        } => commands::query::run(&egg, &query, passphrase.as_deref()),

        Commands::Shell { egg, passphrase } => commands::shell::run(&egg, passphrase.as_deref()),

        Commands::Diff { a, b } => commands::diff::run(&a, &b),

        Commands::Relay {
            egg,
            output,
            zstd_level,
            passphrase,
        } => commands::relay::run(&egg, output.as_deref(), zstd_level, passphrase.as_deref()),

        Commands::Web { egg, port } => commands::web::run(&egg, port),
    };

    if let Err(err) = result {
        eprintln!("{}: {:#}", colored::Colorize::red("Error"), err);
        process::exit(1);
    }
}
