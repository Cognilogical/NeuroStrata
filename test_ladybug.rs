use lbug::{SystemConfig, Database, Connection};
fn main() {
    let db_path = "/home/kenton/.config/NeuroStrata/data/db";
    let cfg = SystemConfig::default().read_only(true);
    let db = Database::new(db_path, cfg).unwrap();
    println!("Successfully opened read-only database");
}
