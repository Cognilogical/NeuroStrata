use std::env;
use std::fs;
use std::path::Path;

use futures::StreamExt;
use kuzu::{Connection, Database, SystemConfig};
use lancedb::query::ExecutableQuery;
use arrow_array::{FixedSizeListArray, Float32Array, RecordBatch, StringArray};
use arrow_schema::DataType;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <lancedb_path> <kuzu_path>", args[0]);
        std::process::exit(1);
    }

    let lancedb_path = &args[1];
    let kuzu_path = &args[2];

    println!("Migrating from LanceDB: {}", lancedb_path);
    println!("To Kuzu: {}", kuzu_path);

    if !Path::new(lancedb_path).exists() {
        eprintln!("LanceDB path does not exist");
        std::process::exit(1);
    }

    // Initialize Kuzu
    let db = Database::new(kuzu_path, SystemConfig::default())?;
    let conn = Connection::new(&db)?;

    // Initialize LanceDB
    let lance_conn = lancedb::connect(lancedb_path).execute().await?;
    let table_names = lance_conn.table_names().execute().await?;

    if table_names.is_empty() {
        println!("No tables found in LanceDB.");
        return Ok(());
    }

    let mut schema_created = false;
    let mut dimensions = 0;

    for namespace in &table_names {
        println!("Processing namespace: {}", namespace);
        let table = lance_conn.open_table(namespace).execute().await?;
        
        let schema = table.schema().await?;
        let vector_field = schema.field_with_name("vector").expect("Missing vector field in LanceDB");
        
        if let DataType::FixedSizeList(_, dim) = vector_field.data_type() {
            dimensions = *dim;
        } else {
            eprintln!("Unexpected vector column type");
            continue;
        }

        // Create Kuzu Schema if not already done
        if !schema_created {
            let create_memory_table = format!(
                "CREATE NODE TABLE Memory (id STRING, namespace STRING, content STRING, user_id STRING, memory_type STRING, agent_name STRING, location STRING, location_lines STRING, metadata STRING, embedding FLOAT[{}], PRIMARY KEY (id))",
                dimensions
            );
            if let Err(e) = conn.query(&create_memory_table) {
                println!("Warning creating table (might already exist): {:?}", e);
            }
            let create_rel_table = "CREATE REL TABLE RELATES_TO (FROM Memory TO Memory)";
            if let Err(e) = conn.query(create_rel_table) {
                println!("Warning creating rel table (might already exist): {:?}", e);
            }
            schema_created = true;
        }

        let mut stream = table.query().execute().await?;
        
        while let Some(batch) = stream.next().await {
            let batch: RecordBatch = batch?;
            let ids = batch.column_by_name("id").unwrap().as_any().downcast_ref::<StringArray>().unwrap();
            let contents = if let Some(col) = batch.column_by_name("content") {
                col.as_any().downcast_ref::<StringArray>().unwrap()
            } else {
                batch.column_by_name("data").unwrap().as_any().downcast_ref::<StringArray>().unwrap()
            };
            let user_ids_opt = batch.column_by_name("user_id").map(|c| c.as_any().downcast_ref::<StringArray>().unwrap());
            let memory_types_opt = batch.column_by_name("memory_type").map(|c| c.as_any().downcast_ref::<StringArray>().unwrap());
            let agent_names_opt = batch.column_by_name("agent_name").map(|c| c.as_any().downcast_ref::<StringArray>().unwrap());
            let locations_opt = batch.column_by_name("location").map(|c| c.as_any().downcast_ref::<StringArray>().unwrap());
            let location_lines_opt = batch.column_by_name("location_lines").map(|c| c.as_any().downcast_ref::<StringArray>().unwrap());
            let metadatas_opt = batch.column_by_name("metadata").map(|c| c.as_any().downcast_ref::<StringArray>().unwrap());
            let vectors = batch.column_by_name("vector").unwrap().as_any().downcast_ref::<FixedSizeListArray>().unwrap();

            for i in 0..batch.num_rows() {
                let id = ids.value(i).replace("'", "\\'");
                let ns = namespace.replace("'", "\\'");
                let content = contents.value(i).replace("'", "\\'");
                let user_id = user_ids_opt.map(|a| a.value(i)).unwrap_or("user").replace("'", "\\'");
                let memory_type = memory_types_opt.map(|a| a.value(i)).unwrap_or("rule").replace("'", "\\'");
                let agent_name = agent_names_opt.map(|a| a.value(i)).unwrap_or("system").replace("'", "\\'");
                let location = locations_opt.map(|a| a.value(i)).unwrap_or("").replace("'", "\\'");
                let location_lines = location_lines_opt.map(|a| a.value(i)).unwrap_or("").replace("'", "\\'");
                let metadata = metadatas_opt.map(|a| a.value(i)).unwrap_or("{}").replace("'", "\\'");
                
                let float_arr = vectors.value(i).as_any().downcast_ref::<Float32Array>().unwrap().clone();
                let vec: Vec<f32> = float_arr.values().to_vec();
                let vec_str = format!("[{}]", vec.iter().map(|f: &f32| f.to_string()).collect::<Vec<_>>().join(","));

                let insert_query = format!(
                    "MERGE (m:Memory {{id: '{}'}})
                    ON CREATE SET m.namespace = '{}', m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}
                    ON MATCH SET m.namespace = '{}', m.content = '{}', m.user_id = '{}', m.memory_type = '{}', m.agent_name = '{}', m.location = '{}', m.location_lines = '{}', m.metadata = '{}', m.embedding = {}",
                    id, ns, content, user_id, memory_type, agent_name, location, location_lines, metadata, vec_str,
                    ns, content, user_id, memory_type, agent_name, location, location_lines, metadata, vec_str
                );

                if let Err(e) = conn.query(&insert_query) {
                    eprintln!("Failed to insert memory ID {}: {}", id, e);
                }
            }
        }
    }

    // Now, reconstruct edges from `metadata` if they contain `related_to` arrays
    println!("Reconstructing RELATES_TO edges from metadata...");
    let query_all = "MATCH (m:Memory) RETURN m.id, m.metadata";
    if let Ok(result) = conn.query(query_all) {
        for row in result {
            let id: String = format!("{}", row[0]);
            let metadata_str: String = format!("{}", row[1]);
            
            if let Ok(metadata_val) = serde_json::from_str::<serde_json::Value>(&metadata_str) {
                if let Some(related) = metadata_val.get("related_to").and_then(|r| r.as_array()) {
                    for rel in related {
                        if let Some(rel_id) = rel.as_str() {
                            let rel_id_safe = rel_id.replace("'", "\\'");
                            // Create edge if both nodes exist
                            let edge_query = format!(
                                "MATCH (a:Memory {{id: '{}'}}), (b:Memory {{id: '{}'}})
                                MERGE (a)-[:RELATES_TO]->(b)",
                                id.replace("'", "\\'"), rel_id_safe
                            );
                            if let Err(e) = conn.query(&edge_query) {
                                eprintln!("Failed to create edge {} -> {}: {}", id, rel_id, e);
                            }
                        }
                    }
                }
            }
        }
    }

    println!("Migration complete!");
    Ok(())
}
