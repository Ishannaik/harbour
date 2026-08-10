# Theming

harbour's theme system is a Rust port of omp's theme JSON schema. Themes are
JSON files that map semantic tokens to colors, named variables, and glyphs. The
default dark theme is **titanium** (Tokyo Night palette), embedded in the
binary. Custom themes live in `~/.harbour/themes/` and hot-reload.

This document describes the intended implementation: the schema, the full token
table, and the runtime behavior of the theme loader.

## Theme file format

A theme file is a single JSON object with these top-level keys:

| key | type | required | purpose |
| --- | --- | --- | --- |
| `name` | string | yes | theme id; for custom themes must match the file name (`<name>.json`) |
| `colors` | object | yes | token → color-value map (see Token reference) |
| `vars` | object | no | named values, recursively referenceable from `colors` via `$name` |
| `symbols` | object | no | glyph preset + per-key overrides + `spinnerFrames` |
| `export` | object | no | passed through verbatim; reserved for external tooling, ignored by harbour |

### Color value types

Every value in `colors` (and `vars`) is exactly one of:

| form | example | meaning |
| --- | --- | --- |
| hex string | `"#7aa2f7"` | RGB color; `#rgb` shorthand accepted |
| 256-index integer | `240` | ANSI 256 palette index (`38;5;n` / `48;5;n`) |
| var reference | `"$accent"` | resolves recursively through `vars` |
| empty string | `""` | terminal default — leave the cell unset (inherit terminal fg/bg) |

Validation is strict on structure: unknown top-level keys, a non-object
`colors`, malformed hex, and out-of-range indices are loud errors that fall
back to titanium defaults (see Custom themes). Missing tokens are legal — a
token absent from `colors` inherits titanium's value.

## Token reference

harbour reads a curated subset of the full omp schema for its views:

**harbour's view subset** — `bg`, `accent`, `border`, `success`, `error`,
`warning`, `muted`, `dim`, `text`, `selectedBg`, `statusLineBg`.

Every other token is omp-app-specific (the omp TUI's chat / markdown / diff /
thinking / status-line surfaces) and is validated but never read by harbour v1
views. Titanium ships values for the complete set so the schema round-trips.
Values not pinned by the design context below are Tokyo Night palette values
matching omp's titanium.

| token | meaning | titanium default | used by harbour | notes |
| --- | --- | --- | --- | --- |
| `bg` | global background | `#16161e` | yes — all views | base canvas; panels inherit it |
| `accent` | primary accent | `#7aa2f7` | yes — splash, search, downloads | logo draw-in, search-bar gradient + shimmer, focused borders, active tab, progress bars |
| `border` | panel borders | `#4c566a` | yes — search, downloads | rounded `╭╮╰╯` + tee junctions on all panes |
| `borderAccent` | focused-border variant | `#7aa2f7` | no — reserved | validated; not read by v1 views |
| `borderMuted` | subdued border | `#3b4261` | no — reserved | validated; not read by v1 views |
| `success` | positive state | `#9ece6a` | yes — search, downloads | healthy source dots, recently-downloaded entries |
| `error` | failure state | `#f7768e` | yes — search, downloads | offline source dots, engine error banner, `failed` items |
| `warning` | degraded state | `#e0af68` | yes — search, downloads | degraded dots, `missing` / paused-seed items |
| `muted` | secondary text | `#565f89` | yes — search, downloads, help | size/seeders, ETA/speed labels, source tags, keybind hints |
| `dim` | de-emphasized | `240` | yes — search | inactive sidebar groups, disabled tabs, ghost text |
| `text` | primary text | `#c0caf5` | yes — all views | query input, result names, queue items, help body |
| `thinkingText` | thinking-display text | `#a9b1d6` | no — omp-app-specific | omp's thinking panel |
| `selectedBg` | cursor-row background | `#2a2f45` | yes — search, downloads | selected row in results list, sidebar, queue |
| `userMessageBg` | user-message block bg | `#292e42` | no — omp-app-specific | chat surface |
| `customMessageBg` | custom-message block bg | `#28344a` | no — omp-app-specific | chat surface |
| `toolPendingBg` | pending tool block bg | `#3b4261` | no — omp-app-specific | tool-call surface |
| `toolSuccessBg` | succeeded tool block bg | `#1e2e1f` | no — omp-app-specific | tool-call surface |
| `toolErrorBg` | failed tool block bg | `#2e1f22` | no — omp-app-specific | tool-call surface |
| `statusLineBg` | status-line background | `#16161e` | yes — status line | bottom bar; animated colorizers draw on it |
| `userMessageText` | user-message text | `#c0caf5` | no — omp-app-specific | chat surface |
| `customMessageText` | custom-message text | `#c0caf5` | no — omp-app-specific | chat surface |
| `customMessageLabel` | custom-message label | `#7aa2f7` | no — omp-app-specific | chat surface |
| `toolTitle` | tool-call title | `#7aa2f7` | no — omp-app-specific | tool-call surface |
| `toolOutput` | tool output text | `#a9b1d6` | no — omp-app-specific | tool-call surface |
| `mdHeading` | markdown heading | `#7aa2f7` | no — omp-app-specific | omp md renderer |
| `mdLink` | markdown link text | `#7aa2f7` | no — omp-app-specific | omp md renderer |
| `mdLinkUrl` | markdown link URL | `#565f89` | no — omp-app-specific | omp md renderer |
| `mdCode` | inline code | `#9ece6a` | no — omp-app-specific | omp md renderer |
| `mdCodeBlock` | code-block text | `#c0caf5` | no — omp-app-specific | omp md renderer |
| `mdCodeBlockBorder` | code-block border | `#3b4261` | no — omp-app-specific | omp md renderer |
| `mdQuote` | blockquote text | `#a9b1d6` | no — omp-app-specific | omp md renderer |
| `mdQuoteBorder` | blockquote bar | `#3b4261` | no — omp-app-specific | omp md renderer |
| `mdHr` | horizontal rule | `#3b4261` | no — omp-app-specific | omp md renderer |
| `mdListBullet` | list bullets | `#7aa2f7` | no — omp-app-specific | omp md renderer |
| `toolDiffAdded` | diff added line | `#9ece6a` | no — omp-app-specific | omp diff view |
| `toolDiffRemoved` | diff removed line | `#f7768e` | no — omp-app-specific | omp diff view |
| `toolDiffContext` | diff context line | `#a9b1d6` | no — omp-app-specific | omp diff view |
| `syntaxComment` | code comment | `#565f89` | no — omp-app-specific | harbour renders no code blocks in v1 |
| `syntaxKeyword` | code keyword | `#bb9af7` | no — omp-app-specific | values ship for schema parity |
| `syntaxFunction` | function name | `#7aa2f7` | no — omp-app-specific | values ship for schema parity |
| `syntaxVariable` | variable name | `#e0af68` | no — omp-app-specific | values ship for schema parity |
| `syntaxString` | string literal | `#9ece6a` | no — omp-app-specific | values ship for schema parity |
| `syntaxNumber` | numeric literal | `#ff9e64` | no — omp-app-specific | values ship for schema parity |
| `syntaxType` | type name | `#2ac3de` | no — omp-app-specific | values ship for schema parity |
| `syntaxOperator` | operator | `#89ddff` | no — omp-app-specific | values ship for schema parity |
| `syntaxPunctuation` | punctuation | `#9aa5ce` | no — omp-app-specific | values ship for schema parity |
| `thinkingOff` | thinking: off level | `#3b4261` | no — omp-app-specific | omp thinking levels |
| `thinkingMinimal` | thinking: minimal level | `#565f89` | no — omp-app-specific | omp thinking levels |
| `thinkingLow` | thinking: low level | `#7aa2f7` | no — omp-app-specific | omp thinking levels |
| `thinkingMedium` | thinking: medium level | `#e0af68` | no — omp-app-specific | omp thinking levels |
| `thinkingHigh` | thinking: high level | `#ff9e64` | no — omp-app-specific | omp thinking levels |
| `thinkingXhigh` | thinking: xhigh level | `#f7768e` | no — omp-app-specific | omp thinking levels |
| `bashMode` | bash-mode colorizer | `#9ece6a` | no — omp-app-specific | omp mode colorizers |
| `pythonMode` | python-mode colorizer | `#7aa2f7` | no — omp-app-specific | omp mode colorizers |
| `statusLineSep` | status-line separator | `#3b4261` | no — omp-app-specific | omp status segments |
| `statusLineModel` | model segment | `#7aa2f7` | no — omp-app-specific | omp status segments |
| `statusLinePath` | path segment | `#a9b1d6` | no — omp-app-specific | omp status segments |
| `statusLineGitClean` | git clean segment | `#9ece6a` | no — omp-app-specific | omp status segments |
| `statusLineGitDirty` | git dirty segment | `#e0af68` | no — omp-app-specific | omp status segments |
| `statusLineContext` | context segment | `#7aa2f7` | no — omp-app-specific | omp status segments |
| `statusLineSpend` | spend segment | `#e0af68` | no — omp-app-specific | omp status segments |
| `statusLineStaged` | staged segment | `#9ece6a` | no — omp-app-specific | omp status segments |
| `statusLineDirty` | dirty segment | `#e0af68` | no — omp-app-specific | omp status segments |
| `statusLineUntracked` | untracked segment | `#565f89` | no — omp-app-specific | omp status segments |
| `statusLineOutput` | output segment | `#a9b1d6` | no — omp-app-specific | omp status segments |
| `statusLineCost` | cost segment | `#ff9e64` | no — omp-app-specific | omp status segments |
| `statusLineSubagents` | subagents segment | `#89ddff` | no — omp-app-specific | omp status segments |

## vars

`vars` holds named color values that `colors` references with `$name` strings.
Resolution is lazy and recursive:

```json
{
  "vars": { "panel": "#1f2335", "panel_alt": "$panel" },
  "colors": { "statusLineBg": "$panel_alt" }
}
```

- References may chain: `"a": "$b"` where `"b": "$c"` resolves to `c`'s value.
- A reference may target any of the four color value types; the empty string
  (terminal default) is a valid target.
- **Cycle detection**: resolution tracks the active reference stack. If a name
  reappears on the stack, validation fails and the error spells out the cycle
  (`vars.a -> vars.b -> vars.a`) — loud, with fallback to titanium.
- Symbols never reference `vars`; symbol overrides are literal strings.

## symbols

`symbols` controls glyphs: borders, progress-bar fill, health dots, and spinner
frames.

| key | type | meaning |
| --- | --- | --- |
| `preset` | `"unicode"` \| `"nerd"` \| `"ascii"` | default glyph set (`unicode` is the default) |
| per-key override | string | replace a single glyph; the preset covers the rest |
| `spinnerFrames` | string[] | spinner glyph ring |

Preset glyphs:

| glyph | unicode | ascii | nerd |
| --- | --- | --- | --- |
| border corners | `╭ ╮ ╰ ╯` | `+ + + +` | as unicode |
| border lines / tees | `─ │ ┬ ┴ ├ ┤` | `- \| + + + +` | as unicode |
| progress fill / half / empty | `█ ▓ ░` | `# = .` | as unicode |
| health dots online / offline | `● ○` | `* o` | nerd icons |

The keys harbour reads are: `borderTl`, `borderTr`, `borderBl`, `borderBr`,
`borderH`, `borderV`, `borderTeeD`, `borderTeeU`, `borderTeeL`, `borderTeeR`
(panel borders and junctions), `progressFill`, `progressHalf`, `progressEmpty`
(eased download bars), `dotOnline`, `dotOffline` (sidebar source-health dots),
and `spinnerFrames`.

`spinnerFrames` is a ring of single-glyph strings. Status spinners advance every
80 ms (~12.5 fps); activity spinners advance at the 30 fps render cadence
(~33 ms). Progress values ease toward their targets independently of the
spinner, so the two never couple.

## Titanium — the default theme

Titanium is embedded in the binary and is the canonical example of a valid
theme. All tokens ship values so the full omp schema round-trips; only harbour's
subset is read at render time.

```json
{
  "name": "titanium",
  "colors": {
    "bg": "#16161e", "text": "#c0caf5", "accent": "#7aa2f7",
    "border": "#4c566a", "borderAccent": "#7aa2f7", "borderMuted": "#3b4261",
    "success": "#9ece6a", "error": "#f7768e", "warning": "#e0af68",
    "muted": "#565f89", "dim": 240, "thinkingText": "#a9b1d6",
    "selectedBg": "#2a2f45", "userMessageBg": "#292e42", "customMessageBg": "#28344a",
    "toolPendingBg": "#3b4261", "toolSuccessBg": "#1e2e1f", "toolErrorBg": "#2e1f22",
    "statusLineBg": "#16161e",
    "userMessageText": "#c0caf5", "customMessageText": "#c0caf5", "customMessageLabel": "#7aa2f7",
    "toolTitle": "#7aa2f7", "toolOutput": "#a9b1d6",
    "mdHeading": "#7aa2f7", "mdLink": "#7aa2f7", "mdLinkUrl": "#565f89",
    "mdCode": "#9ece6a", "mdCodeBlock": "#c0caf5", "mdCodeBlockBorder": "#3b4261",
    "mdQuote": "#a9b1d6", "mdQuoteBorder": "#3b4261", "mdHr": "#3b4261",
    "mdListBullet": "#7aa2f7",
    "toolDiffAdded": "#9ece6a", "toolDiffRemoved": "#f7768e", "toolDiffContext": "#a9b1d6",
    "syntaxComment": "#565f89", "syntaxKeyword": "#bb9af7", "syntaxFunction": "#7aa2f7",
    "syntaxVariable": "#e0af68", "syntaxString": "#9ece6a", "syntaxNumber": "#ff9e64",
    "syntaxType": "#2ac3de", "syntaxOperator": "#89ddff", "syntaxPunctuation": "#9aa5ce",
    "thinkingOff": "#3b4261", "thinkingMinimal": "#565f89", "thinkingLow": "#7aa2f7",
    "thinkingMedium": "#e0af68", "thinkingHigh": "#ff9e64", "thinkingXhigh": "#f7768e",
    "bashMode": "#9ece6a", "pythonMode": "#7aa2f7",
    "statusLineSep": "#3b4261", "statusLineModel": "#7aa2f7", "statusLinePath": "#a9b1d6",
    "statusLineGitClean": "#9ece6a", "statusLineGitDirty": "#e0af68", "statusLineContext": "#7aa2f7",
    "statusLineSpend": "#e0af68", "statusLineStaged": "#9ece6a", "statusLineDirty": "#e0af68",
    "statusLineUntracked": "#565f89", "statusLineOutput": "#a9b1d6", "statusLineCost": "#ff9e64",
    "statusLineSubagents": "#89ddff"
  },
  "vars": { "panel": "#1f2335", "panel_alt": "$panel" },
  "symbols": {
    "preset": "unicode",
    "spinnerFrames": ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
  },
  "export": {}
}
```

The `dim` token demonstrates the 256-index form (`240`); the rest are hex. The
`vars` block is inert here — it exists to show a valid non-empty form.

## Custom themes

- **Location**: `~/.harbour/themes/<name>.json`
  (Windows: `%USERPROFILE%\.harbour\themes\<name>.json`).
- **Selection**: `theme = "titanium"` in `config.toml` is the default;
  `theme = "<name>"` loads `<name>.json`. A missing file is a loud startup
  error with titanium fallback.
- **Live reload**: a file watcher on the themes directory re-parses and
  re-validates on change. A valid swap applies at the next render frame — no
  restart. An invalid edit keeps the last valid theme and prints a loud error
  (stderr + error banner). If the active theme file itself becomes invalid,
  harbour falls back to titanium defaults until the file is fixed.
- Validation failures name the file, the token, and the reason — never a silent
  partial apply. Theme-schema validation is covered by unit tests: fixture
  themes, var cycles, malformed hex, out-of-range indices, bad presets.

## Color mode detection

Detected once at startup, before the alt-screen:

1. `COLORTERM=truecolor` (case-insensitive; `24bit` also accepted) → truecolor.
2. Else, `WT_SESSION` is set (non-empty) → truecolor — Windows Terminal is
   always truecolor-capable.
3. Else → 256-color.

| mode | emission |
| --- | --- |
| truecolor | hex → `\x1b[38;2;r;g;b`m / `48;2` (bg); 256-index ints → `38;5;n` / `48;5;n` |
| 256-color | hex quantized to the nearest ANSI-256 index (standard 6×6×6 cube + grayscale ramp); index ints pass through; var refs resolve before quantization; empty strings stay empty |

The chosen mode is fixed for the process lifetime. The terminal is restored
unconditionally on exit (alt-screen lifecycle), so no re-probing is needed.

## Writing a custom theme

Worked example — a Tokyo Night → Solarized Dark port:

1. Start from the titanium JSON above and write it to
   `~/.harbour/themes/solarized.json` (or copy an existing theme file).
2. Rename `name` to match the file and retune only harbour's subset; everything
   else inherits titanium:

```json
{
  "name": "solarized",
  "colors": {
    "bg": "#002b36", "text": "#839496", "accent": "#268bd2",
    "border": "#073642", "success": "#859900", "error": "#dc322f",
    "warning": "#b58900", "muted": "#586e75", "dim": 8,
    "selectedBg": "#073642", "statusLineBg": "#002b36"
  },
  "vars": { "panel": "#00313f", "panel_alt": "$panel" },
  "symbols": { "preset": "unicode", "progressFill": "━" }
}
```

3. Save the file. The watcher reloads it; the next rendered frame uses the new
   palette — the search-bar gradient follows `accent`, source-health dots follow
   `success`/`warning`/`error`, and the status line repaints on `statusLineBg`.
   Invalid JSON or a bad token prints a loud error and keeps the previous theme.

## Open questions

- `export` is accepted and ignored by harbour; its contents are not defined yet.
- The nerd preset's exact glyphs for `dotOnline`/`dotOffline` and
  `spinnerFrames` are undecided; unicode is the shipped default.
- Whether future views (watch-mode overlay, phase 6) read `borderAccent`,
  `borderMuted`, or the `syntax*` tokens is undecided.
