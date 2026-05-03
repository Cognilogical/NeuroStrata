use lbug::{Connection, Database, SystemConfig};

fn main() {
    let db_path = shellexpand::tilde("~/.config/NeuroStrata/data/db").to_string();
    let db = match Database::new(&db_path, SystemConfig::default()) {
        Ok(db) => db,
        Err(e) => {
            println!("Error opening database: {}", e);
            return;
        }
    };
    let conn = Connection::new(&db).unwrap();
    
    let query = "MATCH (n:Memory) WHERE n.namespace = 'memories' RETURN n.id, n.content LIMIT 20";
    let mut result = match conn.query(query) {
        Ok(res) => res,
        Err(e) => {
            println!("Query error: {}", e);
            return;
        }
    };
    
    while result.has_next() {
        let row = result.get_next().unwrap();
        let id: String = match row[0].clone().try_into() {
            Ok(val) => val,
            Err(_) => "".to_string()
        };
        let content: String = match row[1].clone().try_into() {
            Ok(val) => val,
            Err(_) => "".to_string()
        };
        println!("ID: {}\nContent: {}\n---", id, content);
    }
}
