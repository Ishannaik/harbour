/**
 * Harbour Serverless Indexer for Cloudflare Workers
 * ==================================================
 * Deploy this on Cloudflare Workers for a 100% free, zero-server indexer.
 * 
 * Deployment:
 * 1. Paste into Cloudflare Workers Dashboard (or `wrangler deploy`).
 * 2. In Harbour's config.toml, set:
 *    indexer_url = "https://your-worker-subdomain.workers.dev"
 */

export default {
  async fetch(request) {
    const url = new URL(request.url);

    // 1. Health check
    if (url.pathname === "/health") {
      return new Response(JSON.stringify({ ok: true }), {
        headers: { "Content-Type": "application/json", "Access-Control-Allow-Origin": "*" }
      });
    }

    // 2. Search endpoint
    if (url.pathname === "/search") {
      const q = (url.searchParams.get("q") || "").trim();
      const query = q || "top100:all";

      try {
        const upstream = `https://apibay.org/q.php?q=${encodeURIComponent(query)}`;
        const resp = await fetch(upstream, {
          headers: { "User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" }
        });

        const items = await resp.json();
        const results = (Array.isArray(items) ? items : [])
          .filter(i => i.id !== "0" && i.info_hash)
          .map(i => ({
            name: i.name || "Unknown",
            info_hash: (i.info_hash || "").toLowerCase(),
            size_bytes: parseInt(i.size) || 0,
            seeders: parseInt(i.seeders) || 0,
            leechers: parseInt(i.leechers) || 0,
            source: "serverless_worker",
            magnet: `magnet:?xt=urn:btih:${i.info_hash}&dn=${encodeURIComponent(i.name || "")}`
          }));

        return new Response(JSON.stringify({
          results,
          sources: [{ id: "serverless_worker", status: "online", count: results.length }]
        }), {
          headers: { "Content-Type": "application/json", "Access-Control-Allow-Origin": "*" }
        });
      } catch (err) {
        return new Response(JSON.stringify({
          results: [],
          sources: [{ id: "serverless_worker", status: "offline", count: 0 }],
          error: String(err)
        }), {
          status: 500,
          headers: { "Content-Type": "application/json", "Access-Control-Allow-Origin": "*" }
        });
      }
    }

    return new Response("Harbour Serverless Indexer Running", { status: 200 });
  }
};
