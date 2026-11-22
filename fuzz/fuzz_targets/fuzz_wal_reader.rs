#![no_main]

use eggdb_core::{parse_wal, replay, ColumnMeta, Encoding, SqliteType, TableMeta, WalHeader};
use libfuzzer_sys::fuzz_target;

/// Three small tables (0, 3, and 16 main-file rows; 1, 2, 3 columns) so
/// replayed operations hit valid, invalid, and out-of-range targets.
fn tables() -> Vec<TableMeta> {
    let column = |i: usize| ColumnMeta {
        name: format!("c{i}"),
        sqlite_type: SqliteType::Integer,
        encoding: Encoding::Raw,
        cardinality: 0,
        null_count: 0,
        yolk_offset: 0,
        yolk_length: 0,
        uncompressed_size: 0,
        dictionary: None,
        min_value: None,
        max_value: None,
    };
    [(0u64, 1usize), (3, 2), (16, 3)]
        .iter()
        .enumerate()
        .map(|(t, &(rows, cols))| TableMeta {
            table_name: format!("t{t}"),
            row_count: rows,
            original_ddl: String::new(),
            columns: (0..cols).map(column).collect(),
            index_definitions: Vec::new(),
        })
        .collect()
}

/// Wrap each chunk (split on 0xFE) in a frame prefix with a correct CRC, so
/// the fuzzer reaches payload decoding instead of stopping at the checksum.
fn framed(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in body.split(|&b| b == 0xFE) {
        let len = (chunk.len() as u32).to_le_bytes();
        let crc = crc32c::crc32c_append(crc32c::crc32c(&len), chunk);
        out.extend_from_slice(&len);
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

fuzz_target!(|data: &[u8]| {
    // Parsing and replay of arbitrary bytes must never panic.
    let Some((&mode, body)) = data.split_first() else {
        return;
    };
    let header = WalHeader {
        base_checksum: 1,
        base_length: 2,
    }
    .to_bytes();
    let log = match mode % 3 {
        0 => body.to_vec(),
        1 => [&header[..], body].concat(),
        _ => [&header[..], &framed(body)].concat(),
    };

    if let Ok(parsed) = parse_wal(&log) {
        let _ = replay(&tables(), parsed.commits());
    }
});
