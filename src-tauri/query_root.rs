use kuzu::{Database, Connection, SystemConfig};
fn main() {
    let db_path = "/home/kenton/.config/NeuroStrata/data/db";
    let cfg = SystemConfig::default().read_only(true);
    let db = Database::new(db_path, cfg).unwrap();
    let conn = Connection::new(&db).unwrap();
    let mut res = conn.query("MATCH (n:Memory) WHERE n.memory_type = 'directory' RETURN n.id LIMIT 3").unwrap();
    while let Some(row) = res.next() {
        println!("{:?}", row[0]);
    }
}
