# Legal

Harbour is a BitTorrent client. It does not ship website scrapers.

Search talks to an indexer you run on localhost. That indexer may ship
these **legal** catalogs (wire ids in quotes):

- `demo` — Blender Foundation open movies (CC-BY): Sintel, Big Buck Bunny,
  Tears of Steel, Elephants Dream, Spring, Cosmos Laundromat
- `archive` — Internet Archive public-domain / CC works you add
- `distro` — official Linux ISO torrents (Ubuntu, Debian, Fedora)

Extra catalogs are files you add under `~/.harbour/catalogs/`. You are
responsible for those feeds.

Do not open pull requests that add third-party torrent indexes.
