import json, ssl, urllib.request, sys, time

TOKEN = open("/home/kenton/.config/neurostrata/daemon.token").read().strip()
URL = "http://127.0.0.1:34343/mcp"
_opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

def call(name=None, args=None):
    body = {"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}
    req = urllib.request.Request(URL, data=json.dumps(body).encode(),
        headers={"Authorization":"Bearer "+TOKEN,"Content-Type":"application/json","Accept":"application/json, text/event-stream"})
    with _opener.open(req, timeout=30) as r:
        d = json.loads(r.read())
    if "error" in d:
        return {"error": d["error"], "_raw": str(d)}
    res = d.get("result",{})
    c = res.get("content") or [{}]
    try:
        return json.loads(c[0].get("text","{}"))
    except Exception:
        return {"_raw": c[0].get("text","")}

# Step 1: Confirm health
print("=== Step 1: health/status ===")
print(json.dumps(call(name="neurostrata-mcp", args={"command":"status"})))

# Check if daemon responds to actual tool calls
print("=== Step 1b: direct MCP tool check ===")
print(json.dumps(call(name="neurostrata_get_snapshot", args={"namespace":"NeuroStrata"})))
