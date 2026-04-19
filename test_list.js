const lancedb = require('vectordb');
async function test() {
  const db = await lancedb.connect(`${process.env.HOME}/.local/share/neurostrata/db`);
  const tables = await db.tableNames();
  console.log("Tables:", tables);
  for (const t of tables) {
    const table = await db.openTable(t);
    const data = await table.search([0.0]).limit(10).execute();
    console.log(`\n--- ${t} ---`);
    for (const row of data) {
       console.log(`ID: ${row.id}`);
       console.log(`Payload: ${row.payload}`);
    }
  }
}
test();
