import lancedb
db = lancedb.connect("/home/kenton/.local/share/neurostrata/db")
tbl = db.open_table("global")
tbl.delete("id = '486b1d72-1193-4340-adc2-5f96214c1d7c'")
print("Deleted bad rule")
