#!/usr/bin/env python3
"""
Harbour Custom Python Indexer Template
======================================
A lightweight, standalone starter indexer in Python for custom or niche torrent sites.

Usage:
  1. pip install -r requirements.txt
  2. python python_indexer.py
  3. Harbour connects to http://127.0.0.1:8765 automatically!
"""

import argparse
from fastapi import FastAPI, Query
import httpx
from bs4 import BeautifulSoup
import uvicorn

app = FastAPI(title="Harbour Python Indexer")

@app.get("/health")
def health():
    """Health check ping called by Harbour."""
    return {"ok": True}

@app.get("/search")
async def search(q: str = Query(default=""), exclude: str = Query(default="")):
    """
    Search endpoint called by Harbour.
    If `q` is empty, return curated trending or popular items.
    """
    results = []
    query = q.strip() if q.strip() else "top100:all"
    
    # -------------------------------------------------------------
    # EXAMPLE: Querying an API or scraping an HTML page with httpx + BeautifulSoup
    # -------------------------------------------------------------
    target_url = f"https://apibay.org/q.php?q={query}"
    headers = {"User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64)"}

    try:
        async with httpx.AsyncClient(timeout=8.0) as client:
            resp = await client.get(target_url, headers=headers)
            if resp.status_code == 200:
                data = resp.json()
                for item in data:
                    if item.get("id") == "0":
                        continue
                    name = item.get("name", "Unknown")
                    info_hash = item.get("info_hash", "").lower()
                    if not info_hash:
                        continue

                    results.append({
                        "name": name,
                        "info_hash": info_hash,
                        "size_bytes": int(item.get("size", 0)),
                        "seeders": int(item.get("seeders", 0)),
                        "leechers": int(item.get("leechers", 0)),
                        "source": "custom_python",
                        "magnet": f"magnet:?xt=urn:btih:{info_hash}&dn={name}"
                    })
    except Exception as e:
        print(f"[indexer] Error querying source: {e}")

    return {
        "results": results,
        "sources": [{"id": "custom_python", "status": "online" if results else "empty", "count": len(results)}]
    }

def main():
    parser = argparse.ArgumentParser(description="Harbour Custom Python Indexer")
    parser.add_argument("--port", type=int, default=8765, help="Port to listen on (default: 8765)")
    args = parser.parse_args()

    print(f"[harbour] Custom Python Indexer active on http://127.0.0.1:{args.port}")
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="info")

if __name__ == "__main__":
    main()
