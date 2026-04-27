use kuzu::{Database, Connection, SystemConfig};

fn main() {
    let db_path = "/home/kenton/.config/NeuroStrata/data/db";
    let cfg = SystemConfig::default().read_only(true);
    let db = Database::new(db_path, cfg).unwrap();
    let conn = Connection::new(&db).unwrap();
    let mut res = conn.query("MATCH (n:Memory) RETURN DISTINCT n.memory_type").unwrap();
    while let Some(row) = res.next() {
        println!("{:?}", row[0]);
    }
}
