use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{Path as AxumPath, State};
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use colored::Colorize;
use serde::Serialize;

use crate::format::{format_ratio, format_size, format_timestamp};

struct AppState {
    egg_data: Vec<u8>,
    file_path: String,
    file_size: u64,
}

pub fn run(egg: &Path, port: u16) -> Result<()> {
    anyhow::ensure!(egg.exists(), "egg file not found: {}", egg.display());

    let data = std::fs::read(egg).with_context(|| format!("failed to read {}", egg.display()))?;
    let file_size = data.len() as u64;

    // Verify it's a valid egg
    let _ = eggdb_core::EggReader::open(&data)
        .with_context(|| format!("failed to parse {}", egg.display()))?;

    let file_name = egg
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| egg.display().to_string());

    let state = Arc::new(AppState {
        egg_data: data,
        file_path: file_name,
        file_size,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/summary", get(summary_handler))
        .route("/api/tables", get(tables_handler))
        .route("/api/table/:name", get(table_detail_handler))
        .route("/api/sample/:name", get(sample_handler))
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    println!("  {} {}", "Egg web viewer:".bold(), egg.display());
    println!("  {} http://{}", "Listening:".bold(), addr);
    println!("  Press Ctrl+C to stop");

    let rt = tokio::runtime::Runtime::new().context("failed to create tokio runtime")?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| anyhow::anyhow!("failed to bind {addr}: {e}"))?;
        axum::serve(listener, app)
            .await
            .map_err(|e| anyhow::anyhow!("server error: {e}"))
    })
}

// --- API types ---

#[derive(Serialize)]
struct SummaryResponse {
    file_path: String,
    file_size: u64,
    file_size_human: String,
    table_count: u32,
    total_rows: u64,
    original_size: u64,
    original_size_human: String,
    compression_ratio: String,
    created_at: String,
    version: u8,
    encrypted: bool,
    row_order_preserved: bool,
}

#[derive(Serialize)]
struct TableSummary {
    name: String,
    row_count: u64,
    column_count: usize,
    total_compressed_size: u64,
    total_uncompressed_size: u64,
}

#[derive(Serialize)]
struct ColumnDetail {
    name: String,
    sqlite_type: String,
    encoding: String,
    cardinality: u64,
    null_count: u64,
    compressed_size: u64,
    uncompressed_size: u64,
    min_value: Option<String>,
    max_value: Option<String>,
}

#[derive(Serialize)]
struct TableDetailResponse {
    name: String,
    row_count: u64,
    ddl: String,
    columns: Vec<ColumnDetail>,
}

#[derive(Serialize)]
struct SampleResponse {
    columns: Vec<String>,
    rows: Vec<Vec<serde_json::Value>>,
}

// --- Handlers ---

async fn index_handler() -> Html<&'static str> {
    Html(include_str!("web_index.html"))
}

async fn summary_handler(State(state): State<Arc<AppState>>) -> Json<SummaryResponse> {
    let reader = eggdb_core::EggReader::open(&state.egg_data).expect("egg already validated");
    let header = reader.header();

    let total_rows: u64 = reader.tables().iter().map(|t| t.row_count).sum();

    Json(SummaryResponse {
        file_path: state.file_path.clone(),
        file_size: state.file_size,
        file_size_human: format_size(state.file_size),
        table_count: header.table_count,
        total_rows,
        original_size: header.original_size,
        original_size_human: format_size(header.original_size),
        compression_ratio: format_ratio(header.original_size, state.file_size),
        created_at: format_timestamp(header.created_at),
        version: header.version,
        encrypted: header.flags.has_encryption(),
        row_order_preserved: header.flags.row_order_preserved(),
    })
}

async fn tables_handler(State(state): State<Arc<AppState>>) -> Json<Vec<TableSummary>> {
    let reader = eggdb_core::EggReader::open(&state.egg_data).expect("egg already validated");

    let tables: Vec<TableSummary> = reader
        .tables()
        .iter()
        .map(|t| {
            let total_compressed: u64 = t.columns.iter().map(|c| c.yolk_length).sum();
            let total_uncompressed: u64 = t.columns.iter().map(|c| c.uncompressed_size).sum();

            TableSummary {
                name: t.table_name.clone(),
                row_count: t.row_count,
                column_count: t.columns.len(),
                total_compressed_size: total_compressed,
                total_uncompressed_size: total_uncompressed,
            }
        })
        .collect();

    Json(tables)
}

async fn table_detail_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<TableDetailResponse>, axum::http::StatusCode> {
    let reader = eggdb_core::EggReader::open(&state.egg_data).expect("egg already validated");

    let table = reader
        .tables()
        .iter()
        .find(|t| t.table_name == name)
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;

    let columns: Vec<ColumnDetail> = table
        .columns
        .iter()
        .map(|c| ColumnDetail {
            name: c.name.clone(),
            sqlite_type: c.sqlite_type.to_string(),
            encoding: c.encoding.to_string(),
            cardinality: c.cardinality,
            null_count: c.null_count,
            compressed_size: c.yolk_length,
            uncompressed_size: c.uncompressed_size,
            min_value: c.min_value.as_ref().map(value_to_display),
            max_value: c.max_value.as_ref().map(value_to_display),
        })
        .collect();

    Ok(Json(TableDetailResponse {
        name: table.table_name.clone(),
        row_count: table.row_count,
        ddl: table.original_ddl.clone(),
        columns,
    }))
}

async fn sample_handler(
    State(state): State<Arc<AppState>>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<SampleResponse>, axum::http::StatusCode> {
    let reader = eggdb_core::EggReader::open(&state.egg_data).expect("egg already validated");

    let (table_idx, table) = reader
        .tables()
        .iter()
        .enumerate()
        .find(|(_, t)| t.table_name == name)
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;

    let column_names: Vec<String> = table.columns.iter().map(|c| c.name.clone()).collect();
    let num_cols = table.columns.len();
    let sample_count = std::cmp::min(table.row_count as usize, 100);

    // Decode all columns
    let mut decoded_columns: Vec<Vec<eggdb_core::Value>> = Vec::with_capacity(num_cols);
    for col_idx in 0..num_cols {
        let values = reader
            .decode_column(table_idx, col_idx)
            .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
        decoded_columns.push(values);
    }

    // Transpose to rows, limited to sample_count
    let mut rows: Vec<Vec<serde_json::Value>> = Vec::with_capacity(sample_count);
    for row_idx in 0..sample_count {
        let row: Vec<serde_json::Value> = (0..num_cols)
            .map(|col_idx| value_to_json(&decoded_columns[col_idx][row_idx]))
            .collect();
        rows.push(row);
    }

    Ok(Json(SampleResponse {
        columns: column_names,
        rows,
    }))
}

// --- Helpers ---

fn value_to_display(v: &eggdb_core::Value) -> String {
    match v {
        eggdb_core::Value::Null => "NULL".into(),
        eggdb_core::Value::Integer(i) => i.to_string(),
        eggdb_core::Value::Real(f) => f.to_string(),
        eggdb_core::Value::Text(s) => {
            if s.len() > 100 {
                let truncated: String = s.chars().take(100).collect();
                format!("{truncated}...")
            } else {
                s.clone()
            }
        }
        eggdb_core::Value::Blob(b) => format!("({} bytes)", b.len()),
    }
}

fn value_to_json(v: &eggdb_core::Value) -> serde_json::Value {
    match v {
        eggdb_core::Value::Null => serde_json::Value::Null,
        eggdb_core::Value::Integer(i) => serde_json::json!(*i),
        eggdb_core::Value::Real(f) => serde_json::json!(*f),
        eggdb_core::Value::Text(s) => serde_json::json!(s),
        eggdb_core::Value::Blob(b) => {
            let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
            serde_json::json!(format!("hex:{hex}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use bitvec::prelude::*;
    use eggdb_core::encoding::analyzer::{analyze_column, select_encoding};
    use eggdb_core::encoding::null_bitmap;
    use eggdb_core::format::writer::{write_egg, ColumnInput, TableInput, WriteOptions};
    use eggdb_core::value::SqliteType;
    use eggdb_core::{encoder_for, Value};

    fn make_column(name: &str, sqlite_type: SqliteType, values: &[Value]) -> ColumnInput {
        let stats = analyze_column(values);
        let encoding = select_encoding(&stats);
        let null_bitmap: BitVec = values.iter().map(|v| v.is_null()).collect();
        let non_null = null_bitmap::extract_non_null(values, &null_bitmap);
        let encoder = encoder_for(encoding);
        let encoded = encoder.encode(&non_null);

        ColumnInput {
            name: name.into(),
            sqlite_type,
            encoding,
            cardinality: stats.distinct_count as u64,
            null_count: stats.null_count as u64,
            null_bitmap,
            encoded_values: encoded,
            dictionary: None,
            min_value: stats.min_value,
            max_value: stats.max_value,
            pre_compressed: None,
        }
    }

    fn make_test_egg() -> Vec<u8> {
        let ids: Vec<Value> = (1..=10).map(Value::Integer).collect();
        let names: Vec<Value> = (1..=10).map(|i| Value::Text(format!("user_{i}"))).collect();

        let table = TableInput {
            table_name: "users".into(),
            row_count: 10,
            original_ddl: "CREATE TABLE users (id INTEGER, name TEXT)".into(),
            columns: vec![
                make_column("id", SqliteType::Integer, &ids),
                make_column("name", SqliteType::Text, &names),
            ],
            index_definitions: Vec::new(),
        };

        write_egg(&[table], &WriteOptions::default()).unwrap()
    }

    #[test]
    fn app_state_from_valid_egg() {
        let egg_data = make_test_egg();
        let reader = eggdb_core::EggReader::open(&egg_data).unwrap();
        assert_eq!(reader.header().table_count, 1);
        assert_eq!(reader.tables()[0].table_name, "users");
    }

    #[test]
    fn summary_fields_correct() {
        let egg_data = make_test_egg();
        let reader = eggdb_core::EggReader::open(&egg_data).unwrap();
        let header = reader.header();

        let total_rows: u64 = reader.tables().iter().map(|t| t.row_count).sum();
        assert_eq!(total_rows, 10);
        assert_eq!(header.version, 1);
        assert_eq!(header.table_count, 1);
    }

    #[test]
    fn tables_summary_correct() {
        let egg_data = make_test_egg();
        let reader = eggdb_core::EggReader::open(&egg_data).unwrap();

        let tables: Vec<_> = reader
            .tables()
            .iter()
            .map(|t| {
                let total_compressed: u64 = t.columns.iter().map(|c| c.yolk_length).sum();
                let total_uncompressed: u64 = t.columns.iter().map(|c| c.uncompressed_size).sum();
                (
                    t.table_name.clone(),
                    t.row_count,
                    t.columns.len(),
                    total_compressed,
                    total_uncompressed,
                )
            })
            .collect();

        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].0, "users");
        assert_eq!(tables[0].1, 10);
        assert_eq!(tables[0].2, 2);
    }

    #[test]
    fn sample_decode_works() {
        let egg_data = make_test_egg();
        let reader = eggdb_core::EggReader::open(&egg_data).unwrap();

        let values = reader.decode_column(0, 0).unwrap();
        assert_eq!(values.len(), 10);
        assert_eq!(values[0], Value::Integer(1));
        assert_eq!(values[9], Value::Integer(10));
    }

    #[test]
    fn value_to_json_conversion() {
        use super::value_to_json;

        assert_eq!(value_to_json(&Value::Null), serde_json::Value::Null);
        assert_eq!(value_to_json(&Value::Integer(42)), serde_json::json!(42));
        assert_eq!(value_to_json(&Value::Real(3.14)), serde_json::json!(3.14));
        assert_eq!(
            value_to_json(&Value::Text("hello".into())),
            serde_json::json!("hello")
        );

        let blob_json = value_to_json(&Value::Blob(vec![0xDE, 0xAD]));
        assert_eq!(blob_json, serde_json::json!("hex:dead"));
    }

    #[test]
    fn value_to_display_conversion() {
        use super::value_to_display;

        assert_eq!(value_to_display(&Value::Null), "NULL");
        assert_eq!(value_to_display(&Value::Integer(42)), "42");
        assert_eq!(value_to_display(&Value::Real(3.14)), "3.14");
        assert_eq!(value_to_display(&Value::Text("hi".into())), "hi");
        assert_eq!(value_to_display(&Value::Blob(vec![1, 2, 3])), "(3 bytes)");
    }
}
