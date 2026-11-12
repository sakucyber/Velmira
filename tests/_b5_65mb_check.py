
import http.client, json
BASE = "127.0.0.1:8780"
conn = http.client.HTTPConnection(BASE, timeout=180)
size = 65*1024*1024
conn.putrequest("POST", "/api/books?filename=big.txt")
conn.putheader("Content-Type", "application/octet-stream")
conn.putheader("Content-Length", str(size))
conn.endheaders()
chunk = b"x"*(1024*1024)
for _ in range(65):
    conn.send(chunk)
resp = conn.getresponse()
raw = resp.read()
print("status=", resp.status)
print("ctype=", resp.getheader("Content-Type"))
print("body=", raw.decode("utf-8", "replace")[:300])
conn.close()
