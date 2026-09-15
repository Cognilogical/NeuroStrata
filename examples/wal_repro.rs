//! Why `recovering_config()` in src/store/ladybug.rs disables WAL checksums.
//!
//! The engine cannot verify a WAL it wrote itself once the process dies without
//! a clean close: replay restores the catalog and discards every row. Checksums
//! off, the same rows come back. This reproduces that in isolation, with no
//! NeuroStrata code involved, so the setting can be re-checked rather than taken
//! on faith -- and removed once the engine no longer needs it.
//!
//!   cargo run --release --example wal_repro -- write <dir>   # writes, then aborts
//!   cargo run --release --example wal_repro -- read  <dir>   # counts what survived
//!
//! Expect 0 rows with LBUG_TEST_CHECKSUMS=1, and every row with =0. Measured on
//! lbug 0.15.3 (Windows/MSVC) and 0.19.1 (Ubuntu 24.04, gcc 13.3). Bead
//! neurostrata-kug tracks the upstream defect.
//!
//! Standalone reproduction for the lbug WAL replay report.
//!
//!   cargo run --release --example wal_repro -- write  <dir>
//!   cargo run --release --example wal_repro -- read   <dir>
//!   cargo run --release --example wal_repro -- strict <dir>
//!
//! `write` creates a table, inserts rows, proves they are visible, then aborts
//! the process without a clean close -- the same state a SIGKILL leaves.

use lbug::{Connection, Database, SystemConfig};

const DIMENSIONS: usize = 768;
const ROWS: usize = 3;

fn db_path(dir: &str) -> String {
    format!("{}/ladybug.db", dir)
}

fn embedding_literal(seed: usize) -> String {
    let values: Vec<String> = (0..DIMENSIONS)
        .map(|i| format!("{:.3}", (seed as f32 + i as f32) * 0.001))
        .collect();
    format!("[{}]", values.join(","))
}

/// Checksums are on by default; `LBUG_TEST_CHECKSUMS=0` turns them off on both
/// the write and the read side, to separate "the record bytes are wrong" from
/// "the checksum framing rejects them".
fn checksums_enabled() -> bool {
    std::env::var("LBUG_TEST_CHECKSUMS").map(|v| v != "0").unwrap_or(true)
}

fn write(dir: &str) -> anyhow::Result<()> {
    let config = SystemConfig::default().enable_checksums(checksums_enabled());
    println!("writing with enable_checksums({})", checksums_enabled());
    let db = Database::new(db_path(dir), config)?;
    let conn = Connection::new(&db)?;

    conn.query(&format!(
        "CREATE NODE TABLE Memory (id STRING, content STRING, embedding FLOAT[{}], PRIMARY KEY (id))",
        DIMENSIONS
    ))?;

    for i in 0..ROWS {
        conn.query(&format!(
            "CREATE (m:Memory {{id: 'row-{}', content: 'row {} written before the abort', embedding: {}}})",
            i,
            i,
            embedding_literal(i)
        ))?;
    }

    let visible = count(&conn)?;
    println!("wrote {} rows, {} visible before abort", ROWS, visible);
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        println!("  {:?} {} bytes", entry.file_name(), entry.metadata()?.len());
    }

    // No clean close, no checkpoint: exactly what a killed process leaves behind.
    println!("aborting without a clean close");
    std::process::abort();
}

fn count(conn: &Connection) -> anyhow::Result<usize> {
    let result = conn.query("MATCH (m:Memory) RETURN m.id")?;
    Ok(result.into_iter().count())
}

fn read(dir: &str, strict: bool) -> anyhow::Result<()> {
    let config = SystemConfig::default()
        .throw_on_wal_replay_failure(strict)
        .enable_checksums(checksums_enabled());
    println!(
        "opening with throw_on_wal_replay_failure({}), enable_checksums({})",
        strict,
        checksums_enabled()
    );

    let db = match Database::new(db_path(dir), config) {
        Ok(db) => db,
        Err(e) => {
            println!("open FAILED: {}", e);
            return Ok(());
        }
    };
    let conn = Connection::new(&db)?;
    match count(&conn) {
        Ok(n) => println!("open succeeded, table exists, rows recovered: {}", n),
        Err(e) => println!("open succeeded but the table is gone: {}", e),
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        println!("  {:?} {} bytes", entry.file_name(), entry.metadata()?.len());
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let dir = args.get(2).cloned().unwrap_or_default();
    match mode {
        "write" => write(&dir),
        "read" => read(&dir, false),
        "strict" => read(&dir, true),
        _ => {
            eprintln!("usage: wal_repro <write|read|strict> <dir>");
            std::process::exit(2);
        }
    }
}
