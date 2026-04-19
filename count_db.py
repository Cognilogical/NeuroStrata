import lancedb

db = lancedb.connect("/home/kenton/.local/share/neurostrata/db")
tables = db.table_names()
for t in tables:
    try:
        tbl = db.open_table(t)
        count = len(tbl.search([0.0]*768).limit(10000).to_pandas())
        print(f"Table '{t}': {count} memories")
    except Exception as e:
        print(f"Table '{t}': Error - {e}")
