import lancedb
db = lancedb.connect("/home/kenton/.local/share/neurostrata/db")
tbl = db.open_table("NeuroStrata")
res = tbl.search([0.0]*768).limit(10).to_pandas()
print(res.columns)
print(res[['id', 'location', 'metadata']].to_dict(orient='records'))
