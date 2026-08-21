# Harbour — Custom Indexer Guide

This guide explains how to build a custom search indexer for **Harbour**.

Because Harbour is a protocol-neutral client (the Stremio / Jackett model), the client does not scrape any torrent websites directly. Instead, it queries an HTTP indexer implementing a simple 3-endpoint REST API.

You can implement an indexer in **any language** (Python, Node.js, Go, Rust, Ruby) or host it as a serverless function (Cloudflare Workers, AWS Lambda).

---

## The Wire Contract

Your indexer service must expose three `GET` endpoints:

### 1. `GET /search`
Called when a user searches for torrents in Harbour or browses top lists.

#### Query Parameters:
* `q` *(optional)*: Search query string (e.g., `?q=oppenheimer`). If omitted or empty, return curated trending/top torrents.
* `exclude` *(optional)*: Comma-separated list of source IDs disabled by the user in settings (e.g., `?exclude=gameshub,showport`).

#### Response Format (`200 OK`):
```json
{
  "results": [
    {
      "name": "Dune: Part Two (2024) [1080p]",
      "info_hash": "43413b652721e730b4396b75bff42b698f62d475",
      "size_bytes": 3221225472,
      "seeders": 12400,
      "leechers": 85,
      "source": "cinevault",
      "magnet": "magnet:?xt=urn:btih:43413b652721e730b4396b75bff42b698f62d475&dn=Dune+Part+Two",
      "num_files": 1,
      "added": 1711843200
    }
  ],
  "sources": [
    {
      "id": "cinevault",
      "status": "online",
      "count": 1
    }
  ]
}
```

* `info_hash`: 40-character lowercase hexadecimal hash.
* `magnet` *(optional)*: Full magnet link. If omitted (e.g. for lazy-detail-page scraping), Harbour will call `GET /magnet` when the user selects the item.

---

### 2. `GET /magnet`
*(Optional, needed only if `magnet` was omitted from `/search` results)*

#### Query Parameters:
* `hash`: The 40-hex `info_hash`
* `source`: The source ID string

#### Response Format (`200 OK`):
```json
{
  "magnet": "magnet:?xt=urn:btih:43413b652721e730b4396b75bff42b698f62d475&dn=..."
}
```

---

### 3. `GET /health`
Health check endpoint called by Harbour to verify connectivity.

#### Response Format (`200 OK`):
```json
{
  "ok": true
}
```

---

## Reference Implementation: Python (FastAPI)

Here is a complete, working custom indexer written in Python:

```python
from fastapi import FastAPI
import httpx
import uvicorn

app = FastAPI()

@app.get("/health")
def health():
    return {"ok": True}

@app.get("/search")
async def search(q: str = ""):
    query = q if q else "top100:all"
    url = f"https://mirror-api.org/q.php?q={query}"
    
    async with httpx.AsyncClient() as client:
        res = await client.get(url, timeout=5.0)
        items = res.json()
    
    results = []
    for item in items:
        if item.get("id") == "0":
            continue
        results.append({
            "name": item.get("name", "Unknown"),
            "info_hash": item.get("info_hash", "").lower(),
            "size_bytes": int(item.get("size", 0)),
            "seeders": int(item.get("seeders", 0)),
            "leechers": int(item.get("leechers", 0)),
            "source": "vault-index",
            "magnet": f"magnet:?xt=urn:btih:{item.get('info_hash')}&dn={item.get('name')}"
        })
    
    return {
        "results": results,
        "sources": [{"id": "vault-index", "status": "online", "count": len(results)}]
    }

if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=8765)
```

---

## Reference Implementation: Node.js (Express)

```javascript
const express = require('express');
const app = express();

app.get('/health', (req, res) => res.json({ ok: true }));

app.get('/search', async (req, res) => {
  const query = req.query.q || 'top100:all';
  const response = await fetch(`https://mirror-api.org/q.php?q=${encodeURIComponent(query)}`);
  const items = await response.json();

  const results = items
    .filter(i => i.id !== '0')
    .map(i => ({
      name: i.name,
      info_hash: i.info_hash.toLowerCase(),
      size_bytes: parseInt(i.size) || 0,
      seeders: parseInt(i.seeders) || 0,
      leechers: parseInt(i.leechers) || 0,
      source: 'vault-index',
      magnet: `magnet:?xt=urn:btih:${i.info_hash}&dn=${encodeURIComponent(i.name)}`
    }));

  res.json({
    results,
    sources: [{ id: 'vault-index', status: 'online', count: results.length }]
  });
});

app.listen(8765, '127.0.0.1', () => {
  console.log('Custom Harbour indexer listening on http://127.0.0.1:8765');
});
```

---

## Pointing Harbour to Your Indexer

In your Harbour config file (`~/.harbour/config.toml` on Linux/macOS, `%USERPROFILE%\.harbour\config.toml` on Windows):

```toml
indexer_url = "http://127.0.0.1:8765"
```
Or set the environment variable:
```bash
export HARBOUR_INDEXER_URL="http://127.0.0.1:8765"
```
