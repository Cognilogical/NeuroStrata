use kuzu::{Connection, Database, SystemConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db = Database::new(".NeuroStrata/kuzu_db", SystemConfig::default())?;
    let conn = Connection::new(&db)?;
    
    let fish_ids = vec![
        "036aba94-c914-429c-8c29-0eab9cfa3e9e",
        "13b416c1-b633-41f1-98e4-69fe773c5957",
        "217fa9f9-f3b5-4a1b-a4bf-e0c6db72515e",
        "2eb05687-6803-4cd9-80b1-9c605da4e485",
        "3d501a11-f7a0-4e25-9700-a36e8a51333a",
        "56c5afcb-d4a2-4f3d-8317-6b5b046e594f",
        "5c83810f-75c2-42d4-8b39-20e625f813c8",
        "5fa8c584-d541-43ca-b2fc-0f48a1b6993a",
        "7b8cebea-ec51-4eb9-9747-8411a4e88b55",
        "7d1bf198-5fda-4fcd-a9d2-be8b0af69b75",
        "7d2eb588-f3e2-4788-abc6-36a11a441b44",
        "d52b88f1-5ad2-448f-8931-e36e09e13b74",
        "e9571383-167c-4f69-89f7-4b892601d7aa",
        "2494101b-c6e3-4f93-8551-78c2e6425916", // ml training
        "472e3db7-da7c-4735-a131-0df14f09d84c", // ml vision pipeline domain
        "44e4014b-1555-4675-9c97-b3c437ddb73f", // core logic taxonomy
        "47fac5f0-61d0-40e9-b9ea-e22ff611681a", // core taxonomy domain
        "4398132a-d66a-4632-a583-a9d0fccce284", // data ingestion constraints
        "85698fa5-0b04-4cc4-aaec-4279b98687a4", // nibble frontend
        "c9e55fca-d231-41d1-9f93-c4ecb819f727", // nibble server
        "f8538c8d-3d44-4860-91a7-20d0f2f354ab", // nibble postgres
        "9e0738ea-ddcd-4866-abf2-95f212f4581c", // offline first sqlite
        "b2b47684-25e9-4e74-affb-e85d968593cc", // worms API resolution
        "a30b41ca-ab74-4b53-9d04-63306db09852", // see no evil transfer
        "359b0d92-7473-455d-ab9f-79658e727e02", // key west health
        "e45d047c-78cc-4d3f-b649-1634b82d0d0f", // key west health autopilot
        "fa075b4e-4f3d-4c3e-b851-f76535d4d3d3", // key west health SEO
        "85dfb706-03c2-482a-a9a5-7db8b7baabfe", // key west health SSG 11ty
        "225ff898-ab48-435d-8547-06da62810ce1", // key west health social media
        "8ebd885b-b9f1-4db9-8d48-3a22847a998d", // key west health white linen
        "04b3252e-fb56-4bba-acc4-e46126600c3b", // key west health typography
        "8e4690eb-1f92-498c-bb7f-2b0e6e30ebfb", // key west health design automation
        "0fc4f0ac-f761-4606-ba7e-3ce1bc3120e2", // wix to astro
        "41b5ad98-fb86-4f70-a3e2-8984da0cf0bd", // wix to astro tailwind
        "399d5900-562a-4648-bccf-864a789efbd3", // harrts fuesion x
        "8783293d-ddc5-4ad9-a720-7f28731505c0", // praxis builds skill
        "6de98ac1-cd18-47be-ae8c-1e63968b556b", // praxis gap analysis
        "c52a40e1-7d43-460b-8dff-9b439cba916c", // praxis linkedin deep harvest
        "94890281-a461-4d37-9759-dd9e2b10a905", // praxis build iterative
        "e3d3ba04-eb67-4a0b-b1eb-6062f65a1215", // praxis voice profiling
        "e9e5896f-c85d-4f16-95b8-d21a28a3064e", // praxis contextual researcher
        "aab71e2c-f608-41a2-8964-b5abfb0c1598", // praxis exhaustive authority doc
        "27b1b7c3-30b1-4f1c-96ca-0268393c83b8", // praxis github data export
        "824340ee-11bc-4389-9a76-2e8c20d750ed"  // praxis strict 2 agent loop
    ];

    println!("Mapping Fish Nodes...");
    for id in &fish_ids[0..24] {
        conn.query(&format!("MATCH (m:Memory {{id: '{}'}}) SET m.namespace = 'fish'", id))?;
    }
    
    println!("Mapping Key West Health Nodes...");
    for id in &fish_ids[24..32] {
        conn.query(&format!("MATCH (m:Memory {{id: '{}'}}) SET m.namespace = 'key-west-health'", id))?;
    }
    
    println!("Mapping Wix to Astro Nodes...");
    for id in &fish_ids[32..35] {
        conn.query(&format!("MATCH (m:Memory {{id: '{}'}}) SET m.namespace = 'wix-astro-migration'", id))?;
    }

    println!("Mapping Praxis Nodes...");
    for id in &fish_ids[35..] {
        conn.query(&format!("MATCH (m:Memory {{id: '{}'}}) SET m.namespace = 'praxis'", id))?;
    }

    // Whatever is still left in "memories" is actually a global rule
    println!("Mapping remaining 'memories' to 'global'...");
    conn.query("MATCH (m:Memory) WHERE m.namespace = 'memories' SET m.namespace = 'global'")?;

    println!("Done!");
    Ok(())
}
