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

    def render(self, path: Path) -> None:
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
        img.save(path, "PNG")
        print(path)


def search() -> Term:
    t = Term()
    t.box(0, 0, COLS, ROWS, "harbour — search")
    t.put(2, 2, "library", MUTED)
    t.put(2, 3, "● Demo", SUCCESS)
    t.put(4, 4, "CC Blender", MUTED)
    t.put(2, 6, "catalogs", MUTED)
    t.put(2, 7, "○ user files", MUTED)
    t.put(4, 8, "~/.harbour/", MUTED)
    for y in range(1, ROWS - 1):
        t.put(22, y, "│", MUTED)
    t.put(24, 2, "╭" + "─" * 80 + "╮", MUTED)
    t.put(26, 2, " search ", ACCENT)
    t.put(24, 3, "│", MUTED)
    t.put(26, 3, "sintel", TEXT, BAR)
    t.put(32, 3, "▌", ACCENT, BAR)
    t.put(24 + 81, 3, "│", MUTED)
    t.put(24, 4, "╰" + "─" * 80 + "╯", MUTED)
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
    t.put(2, ROWS - 2, "localhost:8765", SUCCESS)
    return t


def downloads() -> Term:
    t = Term()
    t.box(0, 0, COLS, ROWS, "harbour — downloads")
    t.put(4, 2, "Downloads", ACCENT)
    t.put(4, 3, "─────────", ACCENT)
    t.put(16, 2, "Seeding", MUTED)
    t.put(4, 5, "Sintel (2010) 1080p  CC-BY 3.0  Blender Foundation", TEXT)
    t.put(4, 6, "demo · 4 peers · down 12.4 MiB/s · eta 1m 12s", MUTED)
    filled = 25
    t.put(4, 7, "█" * filled + "░" * (60 - filled), SUCCESS)
    t.put(66, 7, " 42%   512 / 1.2 GiB", TEXT)
    t.put(4, 10, "recently downloaded", MUTED)
    t.put(4, 11, "Big Buck Bunny (2008) 1080p  CC-BY 3.0", TEXT)
    t.put(4, 12, "demo · finished · 2.1 GiB", MUTED)
    t.put(2, ROWS - 2, "q quit · o open · p pause · x remove · w watch · s seeding", MUTED)
    return t


def settings() -> Term:
    t = Term()
    t.box(0, 0, COLS, ROWS, "harbour — settings")
    rows = [
        ("player", "mpv", True),
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
    t.put(4, ROWS - 2, "up/down move · enter edit/toggle · esc back · q quit", MUTED)
    t.put(4, 18, "Harbour is a BitTorrent client. Catalogs are files you add.", MUTED)
    t.put(4, 19, "Demo titles are Creative Commons (Blender Foundation).", MUTED)
    return t


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    search().render(OUT / "search.png")
    downloads().render(OUT / "downloads.png")
    settings().render(OUT / "settings.png")


if __name__ == "__main__":
    main()
