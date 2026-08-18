# Harbour — Custom Indexer Guide

This guide explains how to build a custom search indexer for **Harbour**, or how to bridge existing tools like **Jackett**, **Prowlarr**, or **Torznab** to Harbour.

Because Harbour is a protocol-neutral client (the Stremio / Jackett model), the client binary does not contain any web scrapers or hardcoded torrent indexes. Instead, it queries a standard HTTP indexer implementing a simple 3-endpoint REST API.

You can implement an indexer in **any language** (Python, JavaScript/TypeScript, Go, Rust, Ruby), run it as a local daemon on `127.0.0.1`, or host it as a serverless worker (Cloudflare Workers, AWS Lambda, Render, Fly.io).

---

## Legal Neutrality & The Addon Architecture

Harbour follows the **MGM v. Grokster / Stremio Safe-Harbor model**:
* **Harbour Client:** 100% legal, open-source BitTorrent protocol engine. It contains **zero scrapers**, **zero trackers**, and **zero media catalogs**.
* **The Indexer:** An independent, user-provided service that transforms search queries into generic metadata JSON.
* **Liability Isolation:** Developers and redistributors of Harbour are strictly providing a neutral networking tool. The user chooses which indexer, tracker, or swarm they connect to.

---

## The Wire Contract

Your indexer service must expose three `GET` endpoints:

### 1. `GET /search`
Called when a user searches for torrents in Harbour or browses top lists.

#### Query Parameters:
* `q` *(optional)*: Search query string (e.g., `?q=ubuntu`). If omitted or empty, return curated trending/top torrents.
* `exclude` *(optional)*: Comma-separated list of source IDs disabled by the user in settings (e.g., `?exclude=fitgirl,eztv`).

#### Response Format (`200 OK`):
```json
{
  "results": [
    {
      "name": "Ubuntu 24.04 LTS Desktop (64-bit)",
      "info_hash": "a1b2c3d4e5f67890123456789abcdef012345678",
      "size_bytes": 6221225472,
      "seeders": 1420,
      "leechers": 12,
      "source": "ubuntu",
      "magnet": "magnet:?xt=urn:btih:a1b2c3d4e5f67890123456789abcdef012345678&dn=Ubuntu+24.04",
      "num_files": 1,
      "added": 1711843200
    }
  ],
  "sources": [
    {
      "id": "ubuntu",
      "status": "online",
      "count": 1
    }
  ]
}
```

* `info_hash`: 40-character lowercase hexadecimal hash.
* `magnet` *(optional)*: Full magnet link. If omitted (for lazy on-demand scraping), Harbour will call `GET /magnet` when the user selects the item.

---

### 2. `GET /magnet`
*(Optional, needed only if `magnet` was omitted from `/search` results)*

#### Query Parameters:
* `hash`: The 40-hex `info_hash`
* `source`: The source ID string

#### Response Format (`200 OK`):
```json
{
  "magnet": "magnet:?xt=urn:btih:a1b2c3d4e5f67890123456789abcdef012345678&dn=Ubuntu"
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

## ⚡ Option 1: Bridging Jackett / Prowlarr (Torznab)

If you already run **Jackett** or **Prowlarr**, you don't need to write scrapers. You can run this lightweight bridge in Python:

```python
"""
harbour-torznab-bridge.py
Bridges local Jackett or Prowlarr Torznab API to Harbour's indexer wire format.
"""
from fastapi import FastAPI
import httpx
import xml.etree.ElementTree as ET
import uvicorn

app = FastAPI()

JACKETT_URL = "http://127.0.0.1:9117"
JACKETT_API_KEY = "YOUR_JACKETT_API_KEY_HERE"

@app.get("/health")
def health():
    return {"ok": True}

@app.get("/search")
async def search(q: str = ""):
    torznab_endpoint = f"{JACKETT_URL}/api/v2.0/indexers/all/results/torznab/api"
    params = {
        "apikey": JACKETT_API_KEY,
        "t": "search" if q else "caps",
        "q": q
    }
    
    async with httpx.AsyncClient() as client:
        res = await client.get(torznab_endpoint, params=params, timeout=10.0)
        
    root = ET.fromstring(res.text)
    channel = root.find("channel")
    results = []
    
    if channel is not None:
        for item in channel.findall("item"):
            title = item.findtext("title", "Unknown")
            info_hash = item.findtext("info_hash") or ""
            size = int(item.findtext("size", "0"))
            
            # Extract torznab attributes
            seeders = 0
            for attr in item.findall("{http://torznab.com/schemas/2015/feed}attr"):
                if attr.get("name") == "seeders":
                    seeders = int(attr.get("value", "0"))
                elif attr.get("name") == "infohash" and not info_hash:
                    info_hash = attr.get("value", "")

            magnet = item.findtext("link", "")
            if not magnet.startswith("magnet:") and info_hash:
                magnet = f"magnet:?xt=urn:btih:{info_hash}&dn={title}"
                
            if info_hash:
                results.append({
                    "name": title,
                    "info_hash": info_hash.lower(),
                    "size_bytes": size,
                    "seeders": seeders,
                    "leechers": 0,
                    "source": "jackett",
                    "magnet": magnet
                })
                
    return {
        "results": results,
        "sources": [{"id": "jackett", "status": "online", "count": len(results)}]
    }

if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=8765)
```

---

## ☁️ Option 2: Serverless Cloudflare Worker

Deploy a zero-cost indexer on Cloudflare Workers that requires no local daemon:

```javascript
export default {
  async fetch(request) {
    const url = new URL(request.url);
    
    if (url.pathname === "/health") {
      return new Response(JSON.stringify({ ok: true }), {
        headers: { "Content-Type": "application/json" }
      });
    }
    
    if (url.pathname === "/search") {
      const q = url.searchParams.get("q") || "ubuntu";
      
      // Query any public API / feed
      const apiResp = await fetch(`https://apibay.org/q.php?q=${encodeURIComponent(q)}`);
      const items = await apiResp.json();
      
      const results = (Array.isArray(items) ? items : [])
        .filter(i => i.id !== "0")
        .map(i => ({
          name: i.name,
          info_hash: i.info_hash.toLowerCase(),
          size_bytes: parseInt(i.size) || 0,
          seeders: parseInt(i.seeders) || 0,
          leechers: parseInt(i.leechers) || 0,
          source: "tpb",
          magnet: `magnet:?xt=urn:btih:${i.info_hash}&dn=${encodeURIComponent(i.name)}`
        }));
        
      return new Response(JSON.stringify({
        results,
        sources: [{ id: "tpb", status: "online", count: results.length }]
      }), {
        headers: { "Content-Type": "application/json" }
      });
    }
    
    return new Response("Not Found", { status: 404 });
  }
};
```

---

## 🐍 Option 3: Python FastAPI / Node.js Local Daemon

### Python:
```python
from fastapi import FastAPI
import httpx, uvicorn

app = FastAPI()

@app.get("/health")
def health(): return {"ok": True}

@app.get("/search")
async def search(q: str = ""):
    query = q if q else "top100:all"
    async with httpx.AsyncClient() as client:
        res = await client.get(f"https://apibay.org/q.php?q={query}", timeout=5.0)
        items = res.json()
    
    results = [
        {
            "name": i.get("name", "Unknown"),
            "info_hash": i.get("info_hash", "").lower(),
            "size_bytes": int(i.get("size", 0)),
            "seeders": int(i.get("seeders", 0)),
            "leechers": int(i.get("leechers", 0)),
            "source": "tpb",
            "magnet": f"magnet:?xt=urn:btih:{i.get('info_hash')}&dn={i.get('name')}"
        }
        for i in items if i.get("id") != "0"
    ]
    return {
        "results": results,
        "sources": [{"id": "tpb", "status": "online", "count": len(results)}]
    }

if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=8765)
```

---

## Pointing Harbour to Your Indexer

In your Harbour config file (`~/.harbour/config.toml` on Linux/macOS, `%USERPROFILE%\.harbour\config.toml` on Windows):

```toml
indexer_url = "http://127.0.0.1:8765"
# Or point to a remote/serverless worker:
# indexer_url = "https://my-indexer.workers.dev"
```

Or set the environment variable:
```bash
export HARBOUR_INDEXER_URL="http://127.0.0.1:8765"
```
