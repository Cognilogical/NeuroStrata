import json
import subprocess
import uuid

with open('strata_qdrant_dump.json', 'r') as f:
    data = json.load(f)

points = data.get('result', {}).get('points', [])
print(f"Loaded {len(points)} points from Qdrant dump.")

rust_script = """
use std::env;
use std::fs;
use lancedb::connect;
use lancedb::query::ExecutableQuery;
use arrow::array::{StringArray, FixedSizeListArray, Float32Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema};
use arrow::record_batch::RecordBatchIterator;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = format!("{}/.local/share/neurostrata/db", std::env::var("HOME").unwrap());
    let conn = connect(&db_path).execute().await?;

    let json_data = fs::read_to_string("strata_qdrant_dump.json")?;
    let parsed: serde_json::Value = serde_json::from_str(&json_data)?;
    let points = parsed["result"]["points"].as_array().unwrap();

    let schema = Arc::new(ArrowSchema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("vector", DataType::FixedSizeList(
            Arc::new(Field::new("item", DataType::Float32, true)),
            768
        ), false),
        Field::new("content", DataType::Utf8, false),
        Field::new("user_id", DataType::Utf8, false),
        Field::new("agent_name", DataType::Utf8, false),
        Field::new("metadata", DataType::Utf8, false),
    ]));

    let mut namespaces = std::collections::HashMap::new();

    // Group by namespace
    for point in points {
        let namespace = point["payload"]["user_id"].as_str().unwrap_or("global").to_string();
        namespaces.entry(namespace).or_insert_with(Vec::new).push(point);
    }

    for (namespace, pts) in namespaces {
        println!("Migrating {} points to namespace '{}'", pts.len(), namespace);

        let table_names = conn.table_names().execute().await?;
        if !table_names.contains(&namespace) {
            let empty_batch = RecordBatch::new_empty(schema.clone());
            let batches_iter = Box::new(RecordBatchIterator::new(vec![Ok(empty_batch)].into_iter(), schema.clone()));
            conn.create_table(&namespace, batches_iter).execute().await?;
        }
        let table = conn.open_table(&namespace).execute().await?;

        for point in pts {
            let id = point["id"].as_str().unwrap_or_else(|| "");
            let vector: Vec<f32> = point["vector"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
            let content = point["payload"]["data"].as_str().unwrap_or("");
            let user_id = "voya"; // The old user_id was effectively the namespace. Let's just set the owner to a static user for migrated points.
            let agent_name = "legacy_qdrant_migration";
            let metadata = "{}";

            let id_array = Arc::new(StringArray::from(vec![id]));
            let vector_list = Arc::new(FixedSizeListArray::try_new(
                Arc::new(Field::new("item", DataType::Float32, true)),
                768,
                Arc::new(Float32Array::from(vector)),
                None,
            )?);
            let content_array = Arc::new(StringArray::from(vec![content]));
            let user_id_array = Arc::new(StringArray::from(vec![user_id]));
            let agent_name_array = Arc::new(StringArray::from(vec![agent_name]));
            let metadata_array = Arc::new(StringArray::from(vec![metadata]));

            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    id_array,
                    vector_list,
                    content_array,
                    user_id_array,
                    agent_name_array,
                    metadata_array,
                ],
            )?;

            let batches_iter = Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema.clone()));
            table.delete(format!("id = '{}'", id).as_str()).await.ok();
            table.add(batches_iter).execute().await?;
        }
    }

    Ok(())
}
"""
with open('src/bin/migrate.rs', 'w') as f:
    f.write(rust_script)
