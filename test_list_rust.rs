use std::sync::Arc;
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut db = lancedb::connect(format!("{}/.local/share/neurostrata/db", std::env::var("HOME").unwrap()).as_str()).execute().await?;
    let tables = db.table_names().execute().await?;
    for t in tables {
        let table = db.open_table(&t).execute().await?;
        let res = table.query().limit(10).execute().await?;
        println!("Table {}: {:?}", t, res);
    }
    Ok(())
}
