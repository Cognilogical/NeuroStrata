//! Can the Memory table be keyed on (namespace, id) instead of (id)?
//!
//! Ingested node ids are normalised repo-relative paths -- `src`, `README.md`,
//! `src/main.rs` -- so that a rule naming a file forms a GOVERNS edge to that
//! file's node. The Memory table is declared PRIMARY KEY (id). In a database
//! AGENTS.md calls shared and global, that means two projects cannot both hold
//! a node called `src`: the second ingest MERGEs onto the first project's node
//! and, because upsert does ON MATCH SET m.namespace, moves it. Bead
//! neurostrata-qdg; reproduced against the live database on 2026-08-30.
//!
//! Two candidate fixes were on the table. Namespace-qualifying every id keeps
//! the schema and changes every path that resolves one. A composite primary key
//! changes the schema and leaves ids alone. This decides whether the second is
//! even available on lbug 0.15.3 before either is chosen, with no NeuroStrata
//! code involved.
//!
//!   cargo run --release --example composite_pk_repro
//!
//! Each probe prints PASS or FAIL with the engine's own message, so a refusal is
//! reported rather than inferred.

use lbug::{Connection, Database, SystemConfig};

const DIMENSIONS: usize = 768;

fn embedding_literal(seed: usize) -> String {
    let values: Vec<String> = (0..DIMENSIONS)
        .map(|i| format!("{:.3}", (seed as f32 + i as f32) * 0.001))
        .collect();
    format!("[{}]", values.join(","))
}

/// A scratch database directory that does not collide with the real one.
fn scratch(name: &str) -> String {
    let base = std::env::temp_dir().join(format!("lbug_pk_probe_{}", name));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("scratch dir");
    base.join("ladybug.db").to_string_lossy().replace('\\', "/")
}

fn open(path: &str) -> Database {
    Database::new(path, SystemConfig::default()).expect("open database")
}

fn probe(label: &str, result: Result<(), String>) -> bool {
    match result {
        Ok(()) => {
            println!("  PASS  {}", label);
            true
        }
        Err(e) => {
            println!("  FAIL  {}", label);
            println!("        {}", e.replace('\n', "\n        "));
            false
        }
    }
}

fn run(conn: &Connection, q: &str) -> Result<(), String> {
    conn.query(q).map(|_| ()).map_err(|e| e.to_string())
}

fn count(conn: &Connection, q: &str) -> Result<i64, String> {
    let mut r = conn.query(q).map_err(|e| e.to_string())?;
    match r.next() {
        Some(row) => match &row[0] {
            lbug::Value::Int64(n) => Ok(*n),
            other => Err(format!("unexpected count value: {:?}", other)),
        },
        None => Err("query returned no rows".to_string()),
    }
}

fn main() {
    println!("lbug composite primary key probe");
    println!("Memory-shaped table, FLOAT[{}] embedding column.\n", DIMENSIONS);

    // ---- Probe 1: does the engine accept a two-column PRIMARY KEY at all? ----
    println!("1. CREATE NODE TABLE ... PRIMARY KEY (namespace, id)");
    let path = scratch("composite");
    let db = open(&path);
    let conn = Connection::new(&db).expect("connection");

    let composite = format!(
        "CREATE NODE TABLE Memory (id STRING, namespace STRING, content STRING, \
         embedding FLOAT[{}], PRIMARY KEY (namespace, id))",
        DIMENSIONS
    );
    let composite_ok = probe("two-column primary key accepted", run(&conn, &composite));

    if composite_ok {
        // ---- Probe 2: can the same id live in two namespaces? ----
        println!("\n2. the same id in two namespaces");
        let insert = |ns: &str, seed: usize| {
            format!(
                "CREATE (m:Memory {{id: 'src', namespace: '{}', content: 'dir node for {}', \
                 embedding: {}}})",
                ns,
                ns,
                embedding_literal(seed)
            )
        };
        let a = run(&conn, &insert("ProjectA", 1));
        let b = run(&conn, &insert("ProjectB", 2));
        probe("insert id 'src' into ProjectA", a);
        let second_ok = probe("insert id 'src' into ProjectB", b);

        if second_ok {
            match count(&conn, "MATCH (m:Memory) WHERE m.id = 'src' RETURN count(m)") {
                Ok(n) => probe(
                    &format!("both rows coexist (found {})", n),
                    if n == 2 { Ok(()) } else { Err(format!("expected 2, found {}", n)) },
                ),
                Err(e) => probe("count the rows", Err(e)),
            };

            // ---- Probe 3: does MERGE key on both columns? ----
            println!("\n3. MERGE keyed on both columns");
            let merge = format!(
                "MERGE (m:Memory {{namespace: 'ProjectA', id: 'src'}}) \
                 ON MATCH SET m.content = 'updated in place', m.embedding = {}",
                embedding_literal(3)
            );
            probe("MERGE on (namespace, id)", run(&conn, &merge));

            match count(&conn, "MATCH (m:Memory) WHERE m.id = 'src' RETURN count(m)") {
                Ok(n) => probe(
                    &format!("MERGE updated rather than inserted (found {})", n),
                    if n == 2 { Ok(()) } else { Err(format!("expected 2, found {}", n)) },
                ),
                Err(e) => probe("re-count the rows", Err(e)),
            };

            match count(
                &conn,
                "MATCH (m:Memory) WHERE m.namespace = 'ProjectB' AND m.content = 'dir node for ProjectB' RETURN count(m)",
            ) {
                Ok(n) => probe(
                    "the other namespace's row was untouched",
                    if n == 1 { Ok(()) } else { Err(format!("expected 1, found {}", n)) },
                ),
                Err(e) => probe("check the other namespace", Err(e)),
            };
        }
    }

    // ---- Probe 4: the fallback, for comparison ----
    // Even if the composite key is unavailable, dropping `ON MATCH SET
    // m.namespace` stops a collision from RELOCATING a node. It still cannot
    // make two projects coexist, but it turns silent theft into a visible
    // overwrite within one namespace. Worth knowing which of the two we get.
    println!("\n4. single-column key, for comparison");
    let path2 = scratch("single");
    let db2 = open(&path2);
    let conn2 = Connection::new(&db2).expect("connection");
    let single = format!(
        "CREATE NODE TABLE Memory (id STRING, namespace STRING, content STRING, \
         embedding FLOAT[{}], PRIMARY KEY (id))",
        DIMENSIONS
    );
    probe("single-column primary key accepted", run(&conn2, &single));
    let mk = |ns: &str, seed: usize| {
        format!(
            "CREATE (m:Memory {{id: 'src', namespace: '{}', content: 'dir node', embedding: {}}})",
            ns,
            embedding_literal(seed)
        )
    };
    probe("insert id 'src' into ProjectA", run(&conn2, &mk("ProjectA", 1)));
    let dup = run(&conn2, &mk("ProjectB", 2));
    probe(
        "second project REFUSED (a refusal is better than silent theft)",
        match dup {
            Err(e) => {
                println!("        engine said: {}", e.lines().next().unwrap_or("").trim());
                Ok(())
            }
            Ok(()) => Err("the engine accepted a duplicate id -- it did not refuse".to_string()),
        },
    );

    // ---- Probe 5: keep upstream's UUID key, carry the path as a property ----
    // Upstream mints ids as UUIDs, which is why PRIMARY KEY (id) is safe there.
    // If MERGE can key on (namespace, path) without the primary key appearing in
    // the pattern, the edge work gets its path lookup and ids never collide --
    // the schema upstream already has, plus one property.
    println!("\n5. UUID key, path as a property, MERGE on (namespace, path)");
    let path3 = scratch("uuidkey");
    let db3 = open(&path3);
    let conn3 = Connection::new(&db3).expect("connection");
    let uuid_schema = format!(
        "CREATE NODE TABLE Memory (id STRING, namespace STRING, path STRING, content STRING, \
         embedding FLOAT[{}], PRIMARY KEY (id))",
        DIMENSIONS
    );
    probe("schema with a path property", run(&conn3, &uuid_schema));

    let seed_row = |id: &str, ns: &str, seed: usize| {
        format!(
            "CREATE (m:Memory {{id: '{}', namespace: '{}', path: 'src', content: 'dir node', \
             embedding: {}}})",
            id,
            ns,
            embedding_literal(seed)
        )
    };
    probe("path 'src' in ProjectA (uuid-a)", run(&conn3, &seed_row("uuid-a", "ProjectA", 1)));
    probe("path 'src' in ProjectB (uuid-b)", run(&conn3, &seed_row("uuid-b", "ProjectB", 2)));

    match count(&conn3, "MATCH (m:Memory) WHERE m.path = 'src' RETURN count(m)") {
        Ok(n) => probe(
            &format!("both projects hold a 'src' node (found {})", n),
            if n == 2 { Ok(()) } else { Err(format!("expected 2, found {}", n)) },
        ),
        Err(e) => probe("count", Err(e)),
    };

    // The question that decides the design: can MERGE key on properties that
    // are not the primary key?
    let merge_on_props = format!(
        "MERGE (m:Memory {{namespace: 'ProjectA', path: 'src'}}) \
         ON MATCH SET m.content = 'updated in place', m.embedding = {}",
        embedding_literal(3)
    );
    let merge_ok = probe(
        "MERGE on (namespace, path), no primary key in the pattern",
        run(&conn3, &merge_on_props),
    );

    if merge_ok {
        match count(&conn3, "MATCH (m:Memory) WHERE m.path = 'src' RETURN count(m)") {
            Ok(n) => probe(
                &format!("it updated rather than inserted (found {})", n),
                if n == 2 { Ok(()) } else { Err(format!("expected 2, found {}", n)) },
            ),
            Err(e) => probe("re-count", Err(e)),
        };
        match count(
            &conn3,
            "MATCH (m:Memory) WHERE m.namespace = 'ProjectA' AND m.content = 'updated in place' RETURN count(m)",
        ) {
            Ok(n) => probe(
                "the right project's row was the one updated",
                if n == 1 { Ok(()) } else { Err(format!("expected 1, found {}", n)) },
            ),
            Err(e) => probe("check which row changed", Err(e)),
        };
        match count(
            &conn3,
            "MATCH (m:Memory) WHERE m.namespace = 'ProjectB' AND m.content = 'dir node' RETURN count(m)",
        ) {
            Ok(n) => probe(
                "the other project was untouched",
                if n == 1 { Ok(()) } else { Err(format!("expected 1, found {}", n)) },
            ),
            Err(e) => probe("check the other project", Err(e)),
        };
    }

    // ---- Probe 6: the cheap option -- namespace-qualified ids ----
    println!("\n6. namespace-qualified ids, schema unchanged");
    let path4 = scratch("qualified");
    let db4 = open(&path4);
    let conn4 = Connection::new(&db4).expect("connection");
    probe("single-column schema", run(&conn4, &single));
    let qualified = |ns: &str, seed: usize| {
        format!(
            "CREATE (m:Memory {{id: '{}::src', namespace: '{}', content: 'dir node', \
             embedding: {}}})",
            ns,
            ns,
            embedding_literal(seed)
        )
    };
    probe("ProjectA::src", run(&conn4, &qualified("ProjectA", 1)));
    probe("ProjectB::src", run(&conn4, &qualified("ProjectB", 2)));
    match count(&conn4, "MATCH (m:Memory) WHERE m.id ENDS WITH '::src' RETURN count(m)") {
        Ok(n) => probe(
            &format!("both coexist (found {})", n),
            if n == 2 { Ok(()) } else { Err(format!("expected 2, found {}", n)) },
        ),
        Err(e) => probe("count", Err(e)),
    };
    // Rules stored before such a change name the bare form. Can they resolve?
    match count(
        &conn4,
        "MATCH (m:Memory) WHERE m.namespace = 'ProjectA' AND m.id ENDS WITH '::src' RETURN count(m)",
    ) {
        Ok(n) => probe(
            "a bare 'src' declaration still resolves by suffix within one namespace",
            if n == 1 { Ok(()) } else { Err(format!("expected 1, found {}", n)) },
        ),
        Err(e) => probe("suffix resolution", Err(e)),
    };

    println!("\nDone. Scratch databases are under {}", std::env::temp_dir().display());
}
