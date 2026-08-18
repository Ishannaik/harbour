# Harbour Indexer Tools & Templates

Ready-to-use indexer templates for **Harbour**. Pick whichever fits your workflow:

---

## 1. Jackett / Prowlarr Torznab Bridge (`torznab_bridge.py`)
Connects Harbour directly to your local **Jackett** or **Prowlarr** instance so you can search 500+ private and public trackers without writing scrapers.

### Setup:
```bash
cd tools
pip install -r requirements.txt

# Run the bridge (points to Jackett on port 9117 by default)
python torznab_bridge.py --jackett-url http://127.0.0.1:9117 --api-key YOUR_JACKETT_KEY
```

---

## 2. Custom Python Starter Indexer (`python_indexer.py`)
A fast, standalone Python indexer using `FastAPI` and `httpx` + `BeautifulSoup`. Ideal for writing custom scrapers or testing new sources in 10 lines of code.

### Setup:
```bash
cd tools
pip install -r requirements.txt

# Run on port 8765
python python_indexer.py
```

---

## 3. Serverless Cloudflare Worker (`cloudflare_worker.js`)
Host a completely free, zero-server indexer on Cloudflare Workers.

### Setup:
1. Copy `cloudflare_worker.js` into your Cloudflare Workers Dashboard (or deploy with Wrangler).
2. Set your worker URL in Harbour's `~/.harbour/config.toml`:
   ```toml
   indexer_url = "https://your-worker.workers.dev"
   ```
