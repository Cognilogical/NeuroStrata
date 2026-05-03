// Database Migration Script from Kuzu to LadybugDB

use std::fs;
use std::path::Path;
use kuzu::{Connection as KuzuConnection, Database as KuzuDatabase, SystemConfig};
use lbug::{Connection as LbugConnection, Database as LbugDatabase};

fn migrate_kuzu_to_ladybug(kuzu_db_path: &str, ladybug_db_path: &str) {
    // Open the Kuzu database
    let kuzu_config = SystemConfig::default();
    let kuzu_db = KuzuDatabase::new(kuzu_db_path, kuzu_config).expect("Failed to open Kuzu DB");
    let kuzu_conn = KuzuConnection::new(&kuzu_db);

    // Open the Ladybug database
    if !Path::new(ladybug_db_path).exists() {
        fs::create_dir_all(ladybug_db_path).expect("Failed to create Ladybug DB directory");
    }
    let ladybug_db = LbugDatabase::new(ladybug_db_path, 0).expect("Failed to create Ladybug DB");
    let ladybug_conn = LbugConnection::new(&ladybug_db);

    // Migrate schemas (implement schema-specific logic if needed)
    println!("Migrating schemas...");

    // Migrate data
    println!("Migrating data...");
    // NOTE: Implement specific data migration for your schema
    let kuzu_query = "MATCH (n) RETURN n";
    let kuzu_results = kuzu_conn.execute(kuzu_query).expect("Failed to query Kuzu DB");

    for row in kuzu_results.rows() {
        // Map row to LadybugDB schema (replace the example below with actual logic)
        let node = row.get("node").unwrap();
        let insert_query = format!("INSERT INTO ladybug_nodes VALUES ({:?});", node);
        ladybug_conn.execute(&insert_query).expect("Failed to insert data into Ladybug DB");
    }

    println!("Migration complete.");
}

fn main() {
    let kuzu_path = "~/.config/NeuroStrata/data/db";
    let ladybug_path = "~/.config/NeuroStrata/data/lbug_db";

    migrate_kuzu_to_ladybug(kuzu_path, ladybug_path);
}