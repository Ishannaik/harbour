#!/usr/bin/env python3
"""
Harbour Torznab Bridge (Jackett / Prowlarr)
===========================================
Bridges local Jackett or Prowlarr Torznab XML feeds to Harbour's HTTP wire format.

Usage:
  1. pip install -r requirements.txt
  2. python torznab_bridge.py --jackett-url http://127.0.0.1:9117 --api-key YOUR_KEY
  3. Harbour connects to http://127.0.0.1:8765 automatically!
"""

import argparse
import sys
import xml.etree.ElementTree as ET
from fastapi import FastAPI, Query
import httpx
import uvicorn

app = FastAPI(title="Harbour Torznab Bridge")

CONFIG = {
    "jackett_url": "http://127.0.0.1:9117",
    "api_key": "",
    "port": 8765
}

@app.get("/health")
def health():
    return {"ok": True}

@app.get("/search")
async def search(q: str = Query(default=""), exclude: str = Query(default="")):
    if not CONFIG["api_key"]:
        return {
            "results": [],
            "sources": [{"id": "jackett", "status": "offline", "count": 0}],
            "error": "Jackett API key not configured. Start with --api-key YOUR_KEY"
        }

    torznab_url = f"{CONFIG['jackett_url'].rstrip('/')}/api/v2.0/indexers/all/results/torznab/api"
    params = {
        "apikey": CONFIG["api_key"],
        "t": "search" if q.strip() else "caps",
        "q": q.strip()
    }

    try:
        async with httpx.AsyncClient(timeout=10.0) as client:
            resp = await client.get(torznab_url, params=params)
            resp.raise_for_status()
    except Exception as e:
        return {
            "results": [],
            "sources": [{"id": "jackett", "status": "offline", "count": 0}],
            "error": str(e)
        }

    try:
        root = ET.fromstring(resp.text)
    except Exception:
        return {
            "results": [],
            "sources": [{"id": "jackett", "status": "empty", "count": 0}]
        }

    channel = root.find("channel")
    results = []

    if channel is not None:
        for item in channel.findall("item"):
            title = item.findtext("title", "Unknown").strip()
            info_hash = item.findtext("info_hash") or ""
            size = 0
            try:
                size = int(item.findtext("size", "0"))
            except ValueError:
                pass

            seeders = 0
            leechers = 0
            # Parse Torznab XML attributes namespace
            for attr in item.findall("{http://torznab.com/schemas/2015/feed}attr"):
                name = attr.get("name", "")
                val = attr.get("value", "")
                if name == "seeders":
                    seeders = int(val) if val.isdigit() else 0
                elif name == "peers" or name == "leechers":
                    leechers = int(val) if val.isdigit() else 0
                elif name == "infohash" and not info_hash:
                    info_hash = val

            magnet = item.findtext("link", "").strip()
            if not magnet.startswith("magnet:") and info_hash:
                magnet = f"magnet:?xt=urn:btih:{info_hash.lower()}&dn={title}"

            if info_hash:
                results.append({
                    "name": title,
                    "info_hash": info_hash.lower(),
                    "size_bytes": size,
                    "seeders": seeders,
                    "leechers": leechers,
                    "source": "jackett",
                    "magnet": magnet
                })

    return {
        "results": results,
        "sources": [{"id": "jackett", "status": "online", "count": len(results)}]
    }

def main():
    parser = argparse.ArgumentParser(description="Harbour Torznab Bridge for Jackett / Prowlarr")
    parser.add_argument("--jackett-url", default="http://127.0.0.1:9117", help="Base URL of Jackett or Prowlarr")
    parser.add_argument("--api-key", default="", help="Jackett / Prowlarr API key")
    parser.add_argument("--port", type=int, default=8765, help="Port to serve Harbour indexer on (default 8765)")
    args = parser.parse_args()

    CONFIG["jackett_url"] = args.jackett_url
    CONFIG["api_key"] = args.api_key
    CONFIG["port"] = args.port

    print(f"[harbour-bridge] Starting Torznab Bridge on http://127.0.0.1:{args.port}")
    print(f"[harbour-bridge] Forwarding to Jackett at {args.jackett_url}")
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_level="warning")

if __name__ == "__main__":
    main()
