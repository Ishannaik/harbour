"""Rasterize legal README screenshots of the harbour TUI.

Only Creative Commons Blender Foundation titles. No third-party indexes.
"""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

COLS, ROWS = 108, 28
CW, CH = 9, 20
PAD = 18

BG = (0x16, 0x16, 0x1E)
TEXT = (0xC0, 0xCA, 0xF5)
ACCENT = (0x7A, 0xA2, 0xF7)
SUCCESS = (0x9E, 0xCE, 0x6A)
MUTED = (0x56, 0x5F, 0x89)
SELECTED = (0x2A, 0x2F, 0x45)
BAR = (0x1A, 0x1B, 0x26)
HOT = (0xFF, 0xFF, 0xFF)

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "docs" / "images"


def font() -> ImageFont.FreeTypeFont:
    for p in (
        r"C:\\Windows\\Fonts\\CascadiaMono.ttf",
        r"C:\\Windows\\Fonts\\consola.ttf",
    ):
        if Path(p).exists():
            return ImageFont.truetype(p, 16)
    return ImageFont.load_default()


class Term:
    def __init__(self) -> None:
        self.fg = [[TEXT] * COLS for _ in range(ROWS)]
        self.bg = [[BG] * COLS for _ in range(ROWS)]
        self.ch = [[" "] * COLS for _ in range(ROWS)]

    def put(self, x: int, y: int, s: str, fg=TEXT, bg=BG) -> None:
        for i, c in enumerate(s):
            xx = x + i
            if 0 <= y < ROWS and 0 <= xx < COLS:
                self.ch[y][xx] = c
                self.fg[y][xx] = fg
                self.bg[y][xx] = bg

    def fill(self, x: int, y: int, w: int, h: int, bg) -> None:
        for yy in range(y, y + h):
            for xx in range(x, x + w):
                if 0 <= yy < ROWS and 0 <= xx < COLS:
                    self.bg[yy][xx] = bg
                    self.ch[yy][xx] = " "
                    self.fg[yy][xx] = TEXT

    def box(self, x: int, y: int, w: int, h: int, title: str) -> None:
        hline = "─" * (w - 2)
        self.put(x, y, "╭" + hline + "╮", MUTED)
        if title:
            self.put(x + 2, y, f" {title} ", ACCENT)
        for row in range(1, h - 1):
            self.put(x, y + row, "│", MUTED)
            self.put(x + w - 1, y + row, "│", MUTED)
        self.put(x, y + h - 1, "╰" + hline + "╯", MUTED)

    def image(self) -> Image.Image:
        img = Image.new("RGB", (COLS * CW + PAD * 2, ROWS * CH + PAD * 2), (10, 10, 14))
        draw = ImageDraw.Draw(img)
        f = font()
        for y in range(ROWS):
            for x in range(COLS):
                px, py = PAD + x * CW, PAD + y * CH
                draw.rectangle([px, py, px + CW, py + CH], fill=self.bg[y][x])
        for y in range(ROWS):
            x = 0
            while x < COLS:
                xx = x + 1
                while (
                    xx < COLS
                    and self.fg[y][xx] == self.fg[y][x]
                    and self.bg[y][xx] == self.bg[y][x]
                ):
                    xx += 1
                run = "".join(self.ch[y][x:xx])
                px, py = PAD + x * CW, PAD + y * CH
                draw.text((px, py + 1), run, font=f, fill=self.fg[y][x])
                x = xx
        return img

    def render(self, path: Path) -> None:
        self.image().save(path, "PNG")
        print(path)


def shell(title: str) -> Term:
    t = Term()
    t.box(0, 0, COLS, ROWS, title)
    t.put(2, 2, "library", MUTED)
    t.put(2, 3, "● Demo", SUCCESS)
    t.put(14, 3, " on", SUCCESS)
    t.put(4, 4, "CC Blender", MUTED)
    t.put(2, 6, "catalogs", MUTED)
    t.put(2, 7, "○ user files", MUTED)
    t.put(4, 8, "~/.harbour/", MUTED)
    for y in range(1, ROWS - 1):
        t.put(22, y, "│", MUTED)
    t.put(2, ROWS - 2, "localhost:8765", SUCCESS)
    return t


def search_frame(query: str, results: bool, spinner: str = "") -> Term:
    t = shell("harbour — search")
    t.put(24, 2, "╭" + "─" * 80 + "╮", MUTED)
    t.put(26, 2, " search ", ACCENT)
    t.put(24, 3, "│", MUTED)
    shown = query if query else "search torrents…"
    color = TEXT if query else MUTED
    t.put(26, 3, shown, color, BAR)
    t.put(26 + len(shown), 3, "▌", ACCENT, BAR)
    if spinner:
        t.put(100, 3, spinner, ACCENT, BAR)
    t.put(24 + 81, 3, "│", MUTED)
    t.put(24, 4, "╰" + "─" * 80 + "╯", MUTED)
    if results:
        t.put(24, 6, "  name                                      size     seeds  src ", MUTED)
        rows = [
            ("Sintel (2010) 1080p  CC-BY 3.0  Blender", "1.2 GiB", "842", "demo", True),
            ("Big Buck Bunny (2008) 1080p  CC-BY 3.0", "2.1 GiB", "410", "demo", False),
            ("Tears of Steel (2012) 4K  CC-BY 3.0", "1.8 GiB", "290", "demo", False),
            ("Elephants Dream (2006) 1080p  CC-BY", "780 MiB", "156", "demo", False),
        ]
        y = 7
        for name, size, seeds, src, sel in rows:
            bg = SELECTED if sel else BG
            fg_name = HOT if sel else TEXT
            t.fill(24, y, COLS - 25, 1, bg)
            t.put(24, y, "▸ " if sel else "  ", ACCENT if sel else MUTED, bg)
            t.put(27, y, name.ljust(42)[:42], fg_name, bg)
            t.put(70, y, size.rjust(8), MUTED, bg)
            t.put(80, y, seeds.rjust(6), SUCCESS, bg)
            t.put(88, y, src.rjust(6), ACCENT, bg)
            y += 1
        t.put(24, ROWS - 2, "enter watch · d download · shift+P player · q quit", MUTED)
    else:
        t.put(24, 8, "type a title · enter searches the demo catalog", MUTED)
    return t


def search() -> Term:
    return search_frame("sintel", True)


def downloads_frame(pct: int) -> Term:
    t = Term()
    t.box(0, 0, COLS, ROWS, "harbour — downloads")
    t.put(4, 2, "Downloads", ACCENT)
    t.put(4, 3, "─────────", ACCENT)
    t.put(16, 2, "Seeding", MUTED)
    t.put(4, 5, "Sintel (2010) 1080p  CC-BY 3.0  Blender Foundation", TEXT)
    t.put(4, 6, f"demo · 4 peers · down 12.4 MiB/s · eta {max(1, 72 - pct)}s", MUTED)
    filled = max(1, int(60 * pct / 100))
    t.put(4, 7, "█" * filled + "░" * (60 - filled), SUCCESS)
    done = int(1200 * pct / 100)
    t.put(66, 7, f" {pct}%   {done} / 1.2 GiB", TEXT)
    t.put(4, 10, "recently downloaded", MUTED)
    t.put(4, 11, "Big Buck Bunny (2008) 1080p  CC-BY 3.0", TEXT)
    t.put(4, 12, "demo · finished · 2.1 GiB", MUTED)
    t.put(2, ROWS - 2, "dbl-click folder · o open · w watch · p pause · q quit", MUTED)
    return t


def downloads() -> Term:
    return downloads_frame(42)


def settings() -> Term:
    t = Term()
    t.box(0, 0, COLS, ROWS, "harbour — settings")
    rows = [
        ("player", "VLC", True),
        ("theme", "titanium", False),
        ("download dir", "~/Downloads/harbour", False),
        ("seed by default", "on", False),
        ("indexer", "http://127.0.0.1:8765", False),
        ("demo catalog (Sintel, CC-BY)", "on", False),
        ("user catalogs", "~/.harbour/catalogs/", False),
    ]
    y = 3
    for label, value, sel in rows:
        bg = SELECTED if sel else BG
        t.fill(4, y, COLS - 8, 1, bg)
        t.put(4, y, "▸ " if sel else "  ", ACCENT if sel else MUTED, bg)
        t.put(7, y, label.ljust(34), HOT if sel else TEXT, bg)
        color = ACCENT if sel else (SUCCESS if value == "on" else TEXT)
        t.put(42, y, value, color, bg)
        y += 2
    t.put(4, ROWS - 2, "first row: video player · enter pick · esc back", MUTED)
    t.put(4, 18, "Harbour is a BitTorrent client. Catalogs are files you add.", MUTED)
    t.put(4, 19, "Demo titles are Creative Commons (Blender Foundation).", MUTED)
    return t


def dim_under(t: Term) -> None:
    for y in range(ROWS):
        for x in range(COLS):
            r, g, b = t.bg[y][x]
            t.bg[y][x] = (max(10, r // 3), max(10, g // 3), max(12, b // 3))
            fr, fg, fb = t.fg[y][x]
            t.fg[y][x] = (fr // 2, fg // 2, fb // 2)


def overlay(t: Term, title: str, lines: list[tuple[str, tuple]], hint: str) -> Term:
    dim_under(t)
    w, h = 72, min(18, 5 + len(lines))
    x, y = (COLS - w) // 2, (ROWS - h) // 2
    t.fill(x, y, w, h, BAR)
    t.box(x, y, w, h, title)
    yy = y + 2
    for text, color in lines:
        t.put(x + 2, yy, text[: w - 4], color, BAR)
        yy += 1
    t.put(x + 2, y + h - 2, hint[: w - 4], MUTED, BAR)
    return t


def player_picker(selected: int = 0) -> Term:
    t = search_frame("sintel", True)
    opts = [
        ("1  ●  VLC", SUCCESS),
        ("2  ·  mpv", TEXT),
        ("3  ·  Windows Media Player", TEXT),
    ]
    lines: list[tuple[str, tuple]] = [
        ("This is your video app (VLC is the easy one). Click it.", MUTED),
        ("", TEXT),
    ]
    for i, (label, color) in enumerate(opts):
        prefix = "▸ " if i == selected else "  "
        fg = ACCENT if i == selected else color
        lines.append((prefix + label, fg))
    lines.append(("", TEXT))
    lines.append(("path:", MUTED))
    return overlay(
        t,
        "choose a video player",
        lines,
        "click a name · 1-9 pick · c type a path · r refresh · esc",
    )


def now_playing(pct: int) -> Term:
    t = Term()
    t.box(0, 0, COLS, ROWS, "harbour — now playing")
    t.put(4, 3, "Sintel (2010) 1080p  CC-BY 3.0  Blender Foundation", HOT)
    t.put(4, 5, "● stream  http://127.0.0.1:18765/sintel.mp4", SUCCESS)
    t.put(4, 6, "player  VLC", TEXT)
    t.put(4, 7, "sidecar  sintel.en.srt", MUTED)
    filled = max(1, int(70 * pct / 100))
    t.put(4, 10, "█" * filled + "░" * (70 - filled), SUCCESS)
    t.put(4, 11, f"{pct}%   {pct * 12}s / 14:48   4.2 MiB/s   18 peers", TEXT)
    t.put(4, 14, "opens in VLC — harbour keeps the torrent feeding the stream", MUTED)
    t.put(2, ROWS - 2, "q / esc back to the TUI", MUTED)
    return t


def help_frame() -> Term:
    t = search_frame("sintel", True)
    lines = [
        ("how to start", MUTED),
        ("1  Type a name, then press Enter to search", TEXT),
        ("2  Click a result, or press Enter / w to watch", TEXT),
        ("3  First watch: click VLC or mpv (saved next time)", TEXT),
        ("4  d download · Tab downloads · double-click = folder", TEXT),
        ("", TEXT),
        ("keys", MUTED),
        ("enter     search / watch          d     download", TEXT),
        ("shift+P   pick video player       o     open folder", TEXT),
        ("?         this help                q     quit", TEXT),
    ]
    return overlay(t, "help", lines, "any key closes")


def sources_frame(demo_on: bool) -> Term:
    t = search_frame("sintel", True)
    t.put(2, 3, "● Demo" if demo_on else "· Demo", SUCCESS if demo_on else MUTED)
    t.put(14, 3, " on" if demo_on else "off", SUCCESS if demo_on else MUTED)
    t.put(2, 7, "○ user files", MUTED)
    t.put(14, 7, " on", SUCCESS)
    t.put(24, ROWS - 2, "space source · click source on/off · enter watch", MUTED)
    return t


def save_gif(name: str, frames: list[Image.Image], delays: list[int]) -> None:
    path = OUT / name
    pal = [im.convert("P", palette=Image.Palette.ADAPTIVE, colors=40) for im in frames]
    pal[0].save(
        path,
        save_all=True,
        append_images=pal[1:],
        duration=delays,
        loop=0,
        optimize=True,
    )
    print(path, path.stat().st_size)


def demo_gif() -> None:
    frames: list[Image.Image] = []
    delays: list[int] = []
    for i in range(len("sintel") + 1):
        q = "sintel"[:i]
        frames.append(search_frame(q, False).image())
        delays.append(160 if i else 400)
    for spin in "⠋⠙⠹⠸⠼⠴":
        frames.append(search_frame("sintel", False, spin).image())
        delays.append(80)
    frames.append(search_frame("sintel", True).image())
    delays.append(800)
    for pct in (8, 22, 42):
        frames.append(downloads_frame(pct).image())
        delays.append(280)
    save_gif("demo.gif", frames, delays)


def watch_gif() -> None:
    frames = [
        search_frame("sintel", True).image(),
        player_picker(0).image(),
        player_picker(0).image(),
        now_playing(12).image(),
        now_playing(28).image(),
        now_playing(46).image(),
        now_playing(62).image(),
    ]
    delays = [500, 700, 500, 350, 350, 350, 800]
    save_gif("watch.gif", frames, delays)


def player_gif() -> None:
    frames = [
        search_frame("sintel", True).image(),
        player_picker(0).image(),
        player_picker(1).image(),
        player_picker(0).image(),
        settings().image(),
    ]
    delays = [400, 600, 500, 700, 900]
    save_gif("player.gif", frames, delays)


def sources_gif() -> None:
    frames = [
        sources_frame(True).image(),
        sources_frame(False).image(),
        sources_frame(False).image(),
        sources_frame(True).image(),
    ]
    delays = [700, 700, 500, 800]
    save_gif("sources.gif", frames, delays)


def downloads_gif() -> None:
    frames = [downloads_frame(p).image() for p in (6, 18, 32, 48, 64, 82, 100)]
    delays = [220] * 6 + [700]
    save_gif("downloads.gif", frames, delays)


def help_gif() -> None:
    frames = [
        search_frame("sintel", True).image(),
        help_frame().image(),
        help_frame().image(),
        search_frame("sintel", True).image(),
    ]
    delays = [400, 900, 700, 500]
    save_gif("help.gif", frames, delays)


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    search().render(OUT / "search.png")
    downloads().render(OUT / "downloads.png")
    settings().render(OUT / "settings.png")
    player_picker().render(OUT / "player.png")
    now_playing(46).render(OUT / "watch.png")
    help_frame().render(OUT / "help.png")
    demo_gif()
    watch_gif()
    player_gif()
    sources_gif()
    downloads_gif()
    help_gif()


if __name__ == "__main__":
    main()
