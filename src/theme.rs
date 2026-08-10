//! Theme system — a Rust port of omp's theme JSON schema.
//!
//! Spec: `docs/theming.md`. Themes are JSON files mapping semantic tokens to
//! colors (hex, 256-index, `$var` references, or the empty string for terminal
//! default). The default dark theme **titanium** (Tokyo Night palette) is
//! embedded; custom themes in `~/.harbour/themes/<name>.json` overlay it —
//! a token missing from a custom theme inherits titanium's value.
//!
//! Validation is loud: structural errors, malformed hex, out-of-range indices,
//! unknown keys, and `vars` cycles are all errors, never silent partial
//! applies. Missing tokens are the one deliberate exception (inherit).
//!
//! `dead_code` is allowed module-wide: the full 67-token schema and the
//! 256-color emission path (`to_ansi_256`, `rgb_to_ansi_256`, `ColorMode`,
//! `detect_color_mode*`) are staged API — consumed by the search/downloads
//! views and palette quantization in slice 2+. Remove the allow as views land.

#![allow(dead_code)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;

/// A resolved color value — `$var` references are gone by the time a [`Theme`]
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// 24-bit RGB, emitted as `38;2;r;g;b` in truecolor mode.
    Rgb(u8, u8, u8),
    /// ANSI 256-palette index, emitted as `38;5;n`.
    Index(u8),
    /// Empty string in the theme — leave the cell unset (terminal default).
    Default,
}

impl Color {
    /// ratatui consumes this directly; its backend does the final ANSI
    /// emission (including 256-color quantization when asked).
    pub fn to_ratatui(self) -> ratatui::style::Color {
        match self {
            Color::Rgb(r, g, b) => ratatui::style::Color::Rgb(r, g, b),
            Color::Index(n) => ratatui::style::Color::Indexed(n),
            Color::Default => ratatui::style::Color::Reset,
        }
    }

    /// Quantize to the nearest ANSI-256 index for 256-color terminals.
    /// Index values pass through; `Default` is the caller's problem (skip it).
    pub fn to_ansi_256(self) -> u8 {
        match self {
            Color::Index(n) => n,
            Color::Default => 0,
            Color::Rgb(r, g, b) => rgb_to_ansi_256(r, g, b),
        }
    }
}

/// Nearest ANSI-256 index for an RGB triple: the 6×6×6 color cube first, with
/// a grayscale-ramp fallback for low-chroma colors (the standard algorithm).
pub fn rgb_to_ansi_256(r: u8, g: u8, b: u8) -> u8 {
    let max = r.max(g).max(b) as i16;
    let min = r.min(g).min(b) as i16;
    if max - min < 8 {
        // Near-gray: snap to the 24-step grayscale ramp (232..=255), keeping
        // black/white on the cube anchors which every palette defines
        // identically anyway.
        let avg = (r as u16 + g as u16 + b as u16) / 3;
        if avg < 8 {
            return 16;
        }
        if avg > 247 {
            return 231;
        }
        // 8..=247 maps to ramp steps 0..=23 (240-wide span, 24 steps); a 239
        // divisor would yield step 24 (index 256) at avg == 247, which `as u8`
        // truncates to 0 — black instead of light gray (CodeRabbit CR-1).
        return (232 + ((avg as i16 - 8) * 24 / 240)) as u8;
    }
    fn cube(v: u8) -> u8 {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            ((v as i16 - 35) / 40) as u8
        }
    }
    16 + 36 * cube(r) + 6 * cube(g) + cube(b)
}

/// Terminal color capability, fixed for the process lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Truecolor,
    Color256,
}

/// Detection order per `docs/theming.md`: `COLORTERM=truecolor|24bit`, then a
/// non-empty `WT_SESSION` (Windows Terminal is always truecolor), else 256.
/// Pure over explicit values so tests don't touch real env.
pub fn detect_color_mode_from(colorterm: Option<&str>, wt_session: Option<&str>) -> ColorMode {
    if let Some(ct) = colorterm {
        let ct = ct.trim().to_ascii_lowercase();
        if ct == "truecolor" || ct == "24bit" {
            return ColorMode::Truecolor;
        }
    }
    if wt_session.is_some_and(|s| !s.is_empty()) {
        return ColorMode::Truecolor;
    }
    ColorMode::Color256
}

/// Reads the process environment; the pure logic lives in
/// [`detect_color_mode_from`] so it is unit-testable.
pub fn detect_color_mode() -> ColorMode {
    detect_color_mode_from(
        std::env::var("COLORTERM").ok().as_deref(),
        std::env::var("WT_SESSION").ok().as_deref(),
    )
}

/// All color tokens harbour knows — the full omp schema (67 tokens) so any
/// valid omp theme round-trips. Views read the curated subset via the
/// accessor methods below; the rest is validated and carried for parity.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeColors {
    pub bg: Color,
    pub text: Color,
    pub accent: Color,
    pub border: Color,
    pub border_accent: Color,
    pub border_muted: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    pub muted: Color,
    pub dim: Color,
    pub thinking_text: Color,
    pub selected_bg: Color,
    pub user_message_bg: Color,
    pub custom_message_bg: Color,
    pub tool_pending_bg: Color,
    pub tool_success_bg: Color,
    pub tool_error_bg: Color,
    pub status_line_bg: Color,
    pub user_message_text: Color,
    pub custom_message_text: Color,
    pub custom_message_label: Color,
    pub tool_title: Color,
    pub tool_output: Color,
    pub md_heading: Color,
    pub md_link: Color,
    pub md_link_url: Color,
    pub md_code: Color,
    pub md_code_block: Color,
    pub md_code_block_border: Color,
    pub md_quote: Color,
    pub md_quote_border: Color,
    pub md_hr: Color,
    pub md_list_bullet: Color,
    pub tool_diff_added: Color,
    pub tool_diff_removed: Color,
    pub tool_diff_context: Color,
    pub syntax_comment: Color,
    pub syntax_keyword: Color,
    pub syntax_function: Color,
    pub syntax_variable: Color,
    pub syntax_string: Color,
    pub syntax_number: Color,
    pub syntax_type: Color,
    pub syntax_operator: Color,
    pub syntax_punctuation: Color,
    pub thinking_off: Color,
    pub thinking_minimal: Color,
    pub thinking_low: Color,
    pub thinking_medium: Color,
    pub thinking_high: Color,
    pub thinking_xhigh: Color,
    pub bash_mode: Color,
    pub python_mode: Color,
    pub status_line_sep: Color,
    pub status_line_model: Color,
    pub status_line_path: Color,
    pub status_line_git_clean: Color,
    pub status_line_git_dirty: Color,
    pub status_line_context: Color,
    pub status_line_spend: Color,
    pub status_line_staged: Color,
    pub status_line_dirty: Color,
    pub status_line_untracked: Color,
    pub status_line_output: Color,
    pub status_line_cost: Color,
    pub status_line_subagents: Color,
}

macro_rules! view_accessors {
    ($($name:ident => $field:ident;)*) => {
        $(
            /// Curated view token — see `docs/theming.md` token table.
            pub fn $name(&self) -> Color {
                self.$field
            }
        )*
    };
}

impl ThemeColors {
    view_accessors! {
        bg => bg;
        text => text;
        accent => accent;
        border => border;
        success => success;
        error => error;
        warning => warning;
        muted => muted;
        dim => dim;
        selected_bg => selected_bg;
        status_line_bg => status_line_bg;
    }
}

/// Glyph set controlling borders, progress bars, health dots, and spinners.
/// Overrides are owned; preset values are borrowed static strings.
#[derive(Debug, Clone, PartialEq)]
pub struct Symbols {
    pub border_tl: Cow<'static, str>,
    pub border_tr: Cow<'static, str>,
    pub border_bl: Cow<'static, str>,
    pub border_br: Cow<'static, str>,
    pub border_h: Cow<'static, str>,
    pub border_v: Cow<'static, str>,
    pub border_tee_d: Cow<'static, str>,
    pub border_tee_u: Cow<'static, str>,
    pub border_tee_l: Cow<'static, str>,
    pub border_tee_r: Cow<'static, str>,
    pub progress_fill: Cow<'static, str>,
    pub progress_half: Cow<'static, str>,
    pub progress_empty: Cow<'static, str>,
    pub dot_online: Cow<'static, str>,
    pub dot_offline: Cow<'static, str>,
    pub spinner_frames: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Preset {
    Unicode,
    Ascii,
    Nerd,
}

struct PresetGlyphs {
    border_tl: &'static str,
    border_tr: &'static str,
    border_bl: &'static str,
    border_br: &'static str,
    border_h: &'static str,
    border_v: &'static str,
    border_tee_d: &'static str,
    border_tee_u: &'static str,
    border_tee_l: &'static str,
    border_tee_r: &'static str,
    progress_fill: &'static str,
    progress_half: &'static str,
    progress_empty: &'static str,
    dot_online: &'static str,
    dot_offline: &'static str,
}

impl Preset {
    /// Default glyphs per preset (`docs/theming.md` symbols table). The nerd
    /// preset's dots/spinners are undecided in the spec; it inherits unicode
    /// there (theming.md open questions).
    fn glyphs(self) -> PresetGlyphs {
        match self {
            Preset::Unicode | Preset::Nerd => PresetGlyphs {
                border_tl: "╭",
                border_tr: "╮",
                border_bl: "╰",
                border_br: "╯",
                border_h: "─",
                border_v: "│",
                border_tee_d: "┬",
                border_tee_u: "┴",
                border_tee_l: "├",
                border_tee_r: "┤",
                progress_fill: "█",
                progress_half: "▓",
                progress_empty: "░",
                dot_online: "●",
                dot_offline: "○",
            },
            Preset::Ascii => PresetGlyphs {
                border_tl: "+",
                border_tr: "+",
                border_bl: "+",
                border_br: "+",
                border_h: "-",
                border_v: "|",
                border_tee_d: "+",
                border_tee_u: "+",
                border_tee_l: "+",
                border_tee_r: "+",
                progress_fill: "#",
                progress_half: "=",
                progress_empty: ".",
                dot_online: "*",
                dot_offline: "o",
            },
        }
    }
}

const DEFAULT_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A fully validated, resolved theme.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub colors: ThemeColors,
    pub symbols: Symbols,
}

/// Error produced by theme parsing/validation. Always names the offending
/// key/path — loud, never silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeError(pub String);

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for ThemeError {}

/// The embedded default theme (Tokyo Night palette, full 67-token omp schema).
/// Kept as JSON so the shipped binary parses it through the same code path as
/// custom themes — one parser, no divergent defaults.
const TITANIUM_JSON: &str = r##"{
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
}"##;

/// All 67 schema token names, in canonical order — used to reject unknown
/// keys and to build the resolved colors map.
const TOKEN_NAMES: &[&str] = &[
    "bg",
    "text",
    "accent",
    "border",
    "borderAccent",
    "borderMuted",
    "success",
    "error",
    "warning",
    "muted",
    "dim",
    "thinkingText",
    "selectedBg",
    "userMessageBg",
    "customMessageBg",
    "toolPendingBg",
    "toolSuccessBg",
    "toolErrorBg",
    "statusLineBg",
    "userMessageText",
    "customMessageText",
    "customMessageLabel",
    "toolTitle",
    "toolOutput",
    "mdHeading",
    "mdLink",
    "mdLinkUrl",
    "mdCode",
    "mdCodeBlock",
    "mdCodeBlockBorder",
    "mdQuote",
    "mdQuoteBorder",
    "mdHr",
    "mdListBullet",
    "toolDiffAdded",
    "toolDiffRemoved",
    "toolDiffContext",
    "syntaxComment",
    "syntaxKeyword",
    "syntaxFunction",
    "syntaxVariable",
    "syntaxString",
    "syntaxNumber",
    "syntaxType",
    "syntaxOperator",
    "syntaxPunctuation",
    "thinkingOff",
    "thinkingMinimal",
    "thinkingLow",
    "thinkingMedium",
    "thinkingHigh",
    "thinkingXhigh",
    "bashMode",
    "pythonMode",
    "statusLineSep",
    "statusLineModel",
    "statusLinePath",
    "statusLineGitClean",
    "statusLineGitDirty",
    "statusLineContext",
    "statusLineSpend",
    "statusLineStaged",
    "statusLineDirty",
    "statusLineUntracked",
    "statusLineOutput",
    "statusLineCost",
    "statusLineSubagents",
];

const KNOWN_TOP_KEYS: &[&str] = &["name", "colors", "vars", "symbols", "export"];
const KNOWN_SYMBOL_KEYS: &[&str] = &[
    "borderTl",
    "borderTr",
    "borderBl",
    "borderBr",
    "borderH",
    "borderV",
    "borderTeeD",
    "borderTeeU",
    "borderTeeL",
    "borderTeeR",
    "progressFill",
    "progressHalf",
    "progressEmpty",
    "dotOnline",
    "dotOffline",
];

impl Theme {
    /// Parse + validate a theme JSON string. Errors are loud and name the
    /// offending key. The returned theme has every token resolved — `$var`
    /// references and missing-token inheritance are handled here.
    pub fn parse(json: &str) -> Result<Theme, ThemeError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| ThemeError(format!("invalid JSON: {e}")))?;
        let obj = value
            .as_object()
            .ok_or_else(|| ThemeError("theme root must be a JSON object".into()))?;

        for key in obj.keys() {
            if !KNOWN_TOP_KEYS.contains(&key.as_str()) {
                return Err(ThemeError(format!("unknown top-level key `{key}`")));
            }
        }

        let name = match obj.get("name") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(_) => return Err(ThemeError("`name` must be a string".into())),
            None => return Err(ThemeError("missing required key `name`".into())),
        };

        let colors = parse_colors(obj.get("colors"), obj.get("vars"))?;
        let symbols = parse_symbols(obj.get("symbols"))?;

        Ok(Theme {
            name,
            colors,
            symbols,
        })
    }

    /// The titanium theme, parsed through the same validation path as custom
    /// themes (it is guaranteed valid by tests).
    pub fn titanium() -> Theme {
        Theme::parse(TITANIUM_JSON).expect("embedded titanium theme must parse")
    }

    /// Load a custom theme from `<dir>/<name>.json` and overlay it on
    /// titanium. A missing file or a parse error is a loud error with
    /// titanium fallback (caller decides how to surface it).
    pub fn load_custom(dir: &std::path::Path, name: &str) -> Result<Theme, ThemeError> {
        let path = dir.join(format!("{name}.json"));
        let json = std::fs::read_to_string(&path)
            .map_err(|e| ThemeError(format!("cannot read theme file {}: {e}", path.display())))?;
        Self::parse(&json)
    }
}

/// A `ThemeColors` with every token `Default` — the empty baseline for the
/// titanium parse (which overrides every token).
fn zero_colors() -> ThemeColors {
    use ThemeColors as TC;
    TC {
        bg: Color::Default,
        text: Color::Default,
        accent: Color::Default,
        border: Color::Default,
        border_accent: Color::Default,
        border_muted: Color::Default,
        success: Color::Default,
        error: Color::Default,
        warning: Color::Default,
        muted: Color::Default,
        dim: Color::Default,
        thinking_text: Color::Default,
        selected_bg: Color::Default,
        user_message_bg: Color::Default,
        custom_message_bg: Color::Default,
        tool_pending_bg: Color::Default,
        tool_success_bg: Color::Default,
        tool_error_bg: Color::Default,
        status_line_bg: Color::Default,
        user_message_text: Color::Default,
        custom_message_text: Color::Default,
        custom_message_label: Color::Default,
        tool_title: Color::Default,
        tool_output: Color::Default,
        md_heading: Color::Default,
        md_link: Color::Default,
        md_link_url: Color::Default,
        md_code: Color::Default,
        md_code_block: Color::Default,
        md_code_block_border: Color::Default,
        md_quote: Color::Default,
        md_quote_border: Color::Default,
        md_hr: Color::Default,
        md_list_bullet: Color::Default,
        tool_diff_added: Color::Default,
        tool_diff_removed: Color::Default,
        tool_diff_context: Color::Default,
        syntax_comment: Color::Default,
        syntax_keyword: Color::Default,
        syntax_function: Color::Default,
        syntax_variable: Color::Default,
        syntax_string: Color::Default,
        syntax_number: Color::Default,
        syntax_type: Color::Default,
        syntax_operator: Color::Default,
        syntax_punctuation: Color::Default,
        thinking_off: Color::Default,
        thinking_minimal: Color::Default,
        thinking_low: Color::Default,
        thinking_medium: Color::Default,
        thinking_high: Color::Default,
        thinking_xhigh: Color::Default,
        bash_mode: Color::Default,
        python_mode: Color::Default,
        status_line_sep: Color::Default,
        status_line_model: Color::Default,
        status_line_path: Color::Default,
        status_line_git_clean: Color::Default,
        status_line_git_dirty: Color::Default,
        status_line_context: Color::Default,
        status_line_spend: Color::Default,
        status_line_staged: Color::Default,
        status_line_dirty: Color::Default,
        status_line_untracked: Color::Default,
        status_line_output: Color::Default,
        status_line_cost: Color::Default,
        status_line_subagents: Color::Default,
    }
}

/// Build the titanium baseline without recursing through [`Theme::parse`] —
/// this is what breaks the would-be infinite parse loop.
fn titanium_colors() -> ThemeColors {
    let v: serde_json::Value =
        serde_json::from_str(TITANIUM_JSON).expect("embedded titanium JSON must parse");
    parse_colors_impl(v.get("colors"), v.get("vars"), &zero_colors())
        .expect("embedded titanium colors must validate")
}

/// Parse a theme's `colors`+`vars`, inheriting any unset token from titanium.
fn parse_colors(
    colors: Option<&serde_json::Value>,
    vars: Option<&serde_json::Value>,
) -> Result<ThemeColors, ThemeError> {
    parse_colors_impl(colors, vars, &titanium_colors())
}

/// Core overlay: start from `baseline`, apply this theme's `colors` on top,
/// resolving `$var` references against `vars` with cycle detection.
fn parse_colors_impl(
    colors: Option<&serde_json::Value>,
    vars: Option<&serde_json::Value>,
    baseline: &ThemeColors,
) -> Result<ThemeColors, ThemeError> {
    // resolved holds a working copy of the baseline that the overlay mutates.
    let mut resolved: HashMap<&str, Color> = TOKEN_NAMES
        .iter()
        .map(|t| (*t, field(baseline, t)))
        .collect();

    let mut var_map: HashMap<String, serde_json::Value> = HashMap::new();
    if let Some(v) = vars {
        let o = v
            .as_object()
            .ok_or_else(|| ThemeError("`vars` must be an object".into()))?;
        for (k, val) in o {
            if !is_color_value(val) {
                return Err(ThemeError(format!("var `{k}` is not a color value")));
            }
            var_map.insert(k.clone(), val.clone());
        }
    }

    if let Some(c) = colors {
        let o = c
            .as_object()
            .ok_or_else(|| ThemeError("`colors` must be an object".into()))?;
        for (key, val) in o {
            if !TOKEN_NAMES.contains(&key.as_str()) {
                return Err(ThemeError(format!("unknown color token `{key}`")));
            }
            let mut stack: Vec<String> = Vec::new();
            let color = resolve_color_value(key, val, &var_map, &mut stack)?;
            resolved.insert(key.as_str(), color);
        }
    }

    Ok(ThemeColors {
        bg: resolved["bg"],
        text: resolved["text"],
        accent: resolved["accent"],
        border: resolved["border"],
        border_accent: resolved["borderAccent"],
        border_muted: resolved["borderMuted"],
        success: resolved["success"],
        error: resolved["error"],
        warning: resolved["warning"],
        muted: resolved["muted"],
        dim: resolved["dim"],
        thinking_text: resolved["thinkingText"],
        selected_bg: resolved["selectedBg"],
        user_message_bg: resolved["userMessageBg"],
        custom_message_bg: resolved["customMessageBg"],
        tool_pending_bg: resolved["toolPendingBg"],
        tool_success_bg: resolved["toolSuccessBg"],
        tool_error_bg: resolved["toolErrorBg"],
        status_line_bg: resolved["statusLineBg"],
        user_message_text: resolved["userMessageText"],
        custom_message_text: resolved["customMessageText"],
        custom_message_label: resolved["customMessageLabel"],
        tool_title: resolved["toolTitle"],
        tool_output: resolved["toolOutput"],
        md_heading: resolved["mdHeading"],
        md_link: resolved["mdLink"],
        md_link_url: resolved["mdLinkUrl"],
        md_code: resolved["mdCode"],
        md_code_block: resolved["mdCodeBlock"],
        md_code_block_border: resolved["mdCodeBlockBorder"],
        md_quote: resolved["mdQuote"],
        md_quote_border: resolved["mdQuoteBorder"],
        md_hr: resolved["mdHr"],
        md_list_bullet: resolved["mdListBullet"],
        tool_diff_added: resolved["toolDiffAdded"],
        tool_diff_removed: resolved["toolDiffRemoved"],
        tool_diff_context: resolved["toolDiffContext"],
        syntax_comment: resolved["syntaxComment"],
        syntax_keyword: resolved["syntaxKeyword"],
        syntax_function: resolved["syntaxFunction"],
        syntax_variable: resolved["syntaxVariable"],
        syntax_string: resolved["syntaxString"],
        syntax_number: resolved["syntaxNumber"],
        syntax_type: resolved["syntaxType"],
        syntax_operator: resolved["syntaxOperator"],
        syntax_punctuation: resolved["syntaxPunctuation"],
        thinking_off: resolved["thinkingOff"],
        thinking_minimal: resolved["thinkingMinimal"],
        thinking_low: resolved["thinkingLow"],
        thinking_medium: resolved["thinkingMedium"],
        thinking_high: resolved["thinkingHigh"],
        thinking_xhigh: resolved["thinkingXhigh"],
        bash_mode: resolved["bashMode"],
        python_mode: resolved["pythonMode"],
        status_line_sep: resolved["statusLineSep"],
        status_line_model: resolved["statusLineModel"],
        status_line_path: resolved["statusLinePath"],
        status_line_git_clean: resolved["statusLineGitClean"],
        status_line_git_dirty: resolved["statusLineGitDirty"],
        status_line_context: resolved["statusLineContext"],
        status_line_spend: resolved["statusLineSpend"],
        status_line_staged: resolved["statusLineStaged"],
        status_line_dirty: resolved["statusLineDirty"],
        status_line_untracked: resolved["statusLineUntracked"],
        status_line_output: resolved["statusLineOutput"],
        status_line_cost: resolved["statusLineCost"],
        status_line_subagents: resolved["statusLineSubagents"],
    })
}

/// Read one resolved field off a `ThemeColors` by token name. The match is
/// exhaustive over `TOKEN_NAMES`; adding a token to the schema forces a row
/// here by way of the struct field names.
fn field(tc: &ThemeColors, token: &str) -> Color {
    match token {
        "bg" => tc.bg,
        "text" => tc.text,
        "accent" => tc.accent,
        "border" => tc.border,
        "borderAccent" => tc.border_accent,
        "borderMuted" => tc.border_muted,
        "success" => tc.success,
        "error" => tc.error,
        "warning" => tc.warning,
        "muted" => tc.muted,
        "dim" => tc.dim,
        "thinkingText" => tc.thinking_text,
        "selectedBg" => tc.selected_bg,
        "userMessageBg" => tc.user_message_bg,
        "customMessageBg" => tc.custom_message_bg,
        "toolPendingBg" => tc.tool_pending_bg,
        "toolSuccessBg" => tc.tool_success_bg,
        "toolErrorBg" => tc.tool_error_bg,
        "statusLineBg" => tc.status_line_bg,
        "userMessageText" => tc.user_message_text,
        "customMessageText" => tc.custom_message_text,
        "customMessageLabel" => tc.custom_message_label,
        "toolTitle" => tc.tool_title,
        "toolOutput" => tc.tool_output,
        "mdHeading" => tc.md_heading,
        "mdLink" => tc.md_link,
        "mdLinkUrl" => tc.md_link_url,
        "mdCode" => tc.md_code,
        "mdCodeBlock" => tc.md_code_block,
        "mdCodeBlockBorder" => tc.md_code_block_border,
        "mdQuote" => tc.md_quote,
        "mdQuoteBorder" => tc.md_quote_border,
        "mdHr" => tc.md_hr,
        "mdListBullet" => tc.md_list_bullet,
        "toolDiffAdded" => tc.tool_diff_added,
        "toolDiffRemoved" => tc.tool_diff_removed,
        "toolDiffContext" => tc.tool_diff_context,
        "syntaxComment" => tc.syntax_comment,
        "syntaxKeyword" => tc.syntax_keyword,
        "syntaxFunction" => tc.syntax_function,
        "syntaxVariable" => tc.syntax_variable,
        "syntaxString" => tc.syntax_string,
        "syntaxNumber" => tc.syntax_number,
        "syntaxType" => tc.syntax_type,
        "syntaxOperator" => tc.syntax_operator,
        "syntaxPunctuation" => tc.syntax_punctuation,
        "thinkingOff" => tc.thinking_off,
        "thinkingMinimal" => tc.thinking_minimal,
        "thinkingLow" => tc.thinking_low,
        "thinkingMedium" => tc.thinking_medium,
        "thinkingHigh" => tc.thinking_high,
        "thinkingXhigh" => tc.thinking_xhigh,
        "bashMode" => tc.bash_mode,
        "pythonMode" => tc.python_mode,
        "statusLineSep" => tc.status_line_sep,
        "statusLineModel" => tc.status_line_model,
        "statusLinePath" => tc.status_line_path,
        "statusLineGitClean" => tc.status_line_git_clean,
        "statusLineGitDirty" => tc.status_line_git_dirty,
        "statusLineContext" => tc.status_line_context,
        "statusLineSpend" => tc.status_line_spend,
        "statusLineStaged" => tc.status_line_staged,
        "statusLineDirty" => tc.status_line_dirty,
        "statusLineUntracked" => tc.status_line_untracked,
        "statusLineOutput" => tc.status_line_output,
        "statusLineCost" => tc.status_line_cost,
        "statusLineSubagents" => tc.status_line_subagents,
        other => unreachable!("token not in ThemeColors: {other}"),
    }
}

fn is_color_value(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::String(s) => s.is_empty() || s.starts_with('#') || s.starts_with('$'),
        serde_json::Value::Number(n) => n.as_u64().is_some(),
        _ => false,
    }
}

/// Resolve a single color value, chasing `$var` references with cycle
/// detection. `stack` holds the active reference chain for the error message.
fn resolve_color_value(
    key: &str,
    value: &serde_json::Value,
    vars: &HashMap<String, serde_json::Value>,
    stack: &mut Vec<String>,
) -> Result<Color, ThemeError> {
    match value {
        serde_json::Value::String(s) if s.is_empty() => Ok(Color::Default),
        serde_json::Value::String(s) if s.starts_with('$') => {
            let name = &s[1..];
            if stack.iter().any(|n| n == name) {
                let mut cycle: Vec<String> = stack.to_vec();
                cycle.push(name.to_string());
                return Err(ThemeError(format!("vars cycle: {}", cycle.join(" -> "))));
            }
            let target = vars
                .get(name)
                .ok_or_else(|| ThemeError(format!("`{key}` references undefined var `${name}`")))?;
            stack.push(name.to_string());
            let out = resolve_color_value(key, target, vars, stack);
            stack.pop();
            out
        }
        serde_json::Value::String(s) => {
            parse_hex(s).map_err(|e| ThemeError(format!("`{key}`: {e}")))
        }
        serde_json::Value::Number(n) => {
            let idx = n
                .as_u64()
                .ok_or_else(|| ThemeError(format!("`{key}`: index must be an integer")))?;
            if idx > 255 {
                return Err(ThemeError(format!(
                    "`{key}`: index {idx} out of range 0..=255"
                )));
            }
            Ok(Color::Index(idx as u8))
        }
        _ => Err(ThemeError(format!("`{key}`: not a color value"))),
    }
}

/// Parse `#RGB` / `#RRGGBB`.
fn parse_hex(s: &str) -> Result<Color, String> {
    let hex = s
        .strip_prefix('#')
        .ok_or_else(|| format!("`{s}` is not a hex color"))?;
    // Match on the CHARACTER count, not byte length: a multi-byte char such
    // as "…" is 3 bytes but 1 char, so byte-length dispatch would index
    // `.nth(1).unwrap()` past the end and panic. Custom themes are user
    // input — this must produce a loud ThemeError, not abort (CodeRabbit
    // CR-2).
    let chars: Vec<char> = hex.chars().collect();
    let nibble = |c: char| {
        c.to_digit(16)
            .ok_or_else(|| format!("`{s}`: bad hex digit `{c}`"))
    };
    match chars.len() {
        3 => {
            let (r, g, b) = (nibble(chars[0])?, nibble(chars[1])?, nibble(chars[2])?);
            let dup = |v: u32| (v * 16 + v) as u8;
            Ok(Color::Rgb(dup(r), dup(g), dup(b)))
        }
        6 => {
            let pair = |i: usize| -> Result<u8, String> {
                let hi = nibble(chars[i])?;
                let lo = nibble(chars[i + 1])?;
                Ok((hi * 16 + lo) as u8)
            };
            Ok(Color::Rgb(pair(0)?, pair(2)?, pair(4)?))
        }
        n => Err(format!("`{s}`: expected 3 or 6 hex digits, got {n}")),
    }
}

fn parse_symbols(symbols: Option<&serde_json::Value>) -> Result<Symbols, ThemeError> {
    let mut preset = Preset::Unicode;
    let mut overrides: HashMap<String, String> = HashMap::new();
    let mut spinner: Vec<String> = DEFAULT_SPINNER.iter().map(|s| s.to_string()).collect();

    if let Some(s) = symbols {
        let o = s
            .as_object()
            .ok_or_else(|| ThemeError("`symbols` must be an object".into()))?;
        for (key, val) in o {
            match key.as_str() {
                "preset" => {
                    let p = val
                        .as_str()
                        .ok_or_else(|| ThemeError("`symbols.preset` must be a string".into()))?;
                    preset = match p {
                        "unicode" => Preset::Unicode,
                        "ascii" => Preset::Ascii,
                        "nerd" => Preset::Nerd,
                        other => {
                            return Err(ThemeError(format!(
                                "unknown symbol preset `{other}` (expected unicode|nerd|ascii)"
                            )));
                        }
                    };
                }
                "spinnerFrames" => {
                    let frames = val.as_array().ok_or_else(|| {
                        ThemeError("`symbols.spinnerFrames` must be an array".into())
                    })?;
                    if frames.is_empty() {
                        return Err(ThemeError(
                            "`symbols.spinnerFrames` must not be empty".into(),
                        ));
                    }
                    spinner = frames
                        .iter()
                        .map(|f| {
                            f.as_str().map(str::to_string).ok_or_else(|| {
                                ThemeError("`symbols.spinnerFrames` entries must be strings".into())
                            })
                        })
                        .collect::<Result<_, _>>()?;
                }
                other if KNOWN_SYMBOL_KEYS.contains(&other) => {
                    let glyph = val
                        .as_str()
                        .ok_or_else(|| ThemeError(format!("`symbols.{other}` must be a string")))?;
                    if glyph.is_empty() {
                        return Err(ThemeError(format!("`symbols.{other}` must not be empty")));
                    }
                    overrides.insert(other.to_string(), glyph.to_string());
                }
                _ => {
                    // Unknown override keys are ignored per spec (theming.md) —
                    // a loud error would reject forward-compatible themes.
                }
            }
        }
    }

    let g = preset.glyphs();
    let pick = |key: &str, preset: &'static str| -> Cow<'static, str> {
        match overrides.get(key) {
            Some(v) => Cow::Owned(v.clone()),
            None => Cow::Borrowed(preset),
        }
    };

    Ok(Symbols {
        border_tl: pick("borderTl", g.border_tl),
        border_tr: pick("borderTr", g.border_tr),
        border_bl: pick("borderBl", g.border_bl),
        border_br: pick("borderBr", g.border_br),
        border_h: pick("borderH", g.border_h),
        border_v: pick("borderV", g.border_v),
        border_tee_d: pick("borderTeeD", g.border_tee_d),
        border_tee_u: pick("borderTeeU", g.border_tee_u),
        border_tee_l: pick("borderTeeL", g.border_tee_l),
        border_tee_r: pick("borderTeeR", g.border_tee_r),
        progress_fill: pick("progressFill", g.progress_fill),
        progress_half: pick("progressHalf", g.progress_half),
        progress_empty: pick("progressEmpty", g.progress_empty),
        dot_online: pick("dotOnline", g.dot_online),
        dot_offline: pick("dotOffline", g.dot_offline),
        spinner_frames: spinner,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titanium_parses_and_subset_matches_spec() {
        let t = Theme::titanium();
        assert_eq!(t.name, "titanium");
        // Spot-check the curated subset against docs/theming.md.
        assert_eq!(t.colors.accent(), Color::Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(t.colors.success(), Color::Rgb(0x9e, 0xce, 0x6a));
        assert_eq!(t.colors.error(), Color::Rgb(0xf7, 0x76, 0x8e));
        assert_eq!(t.colors.warning(), Color::Rgb(0xe0, 0xaf, 0x68));
        assert_eq!(t.colors.muted(), Color::Rgb(0x56, 0x5f, 0x89));
        assert_eq!(t.colors.dim(), Color::Index(240));
        assert_eq!(t.colors.text(), Color::Rgb(0xc0, 0xca, 0xf5));
        assert_eq!(t.colors.selected_bg(), Color::Rgb(0x2a, 0x2f, 0x45));
        assert_eq!(t.colors.status_line_bg(), Color::Rgb(0x16, 0x16, 0x1e));
        assert_eq!(t.symbols.spinner_frames.len(), 10);
        assert_eq!(t.symbols.border_tl.as_ref(), "╭");
    }

    #[test]
    fn hex_shorthand_and_index_parse() {
        let t = Theme::parse(r##"{"name":"t","colors":{"accent":"#7af","dim":42}}"##).unwrap();
        assert_eq!(t.colors.accent(), Color::Rgb(0x77, 0xaa, 0xff));
        assert_eq!(t.colors.dim(), Color::Index(42));
    }

    #[test]
    fn missing_tokens_inherit_titanium() {
        let t = Theme::parse(r##"{"name":"t","colors":{"accent":"#ff0000"}}"##).unwrap();
        assert_eq!(t.colors.accent(), Color::Rgb(0xff, 0x00, 0x00));
        assert_eq!(t.colors.success(), Theme::titanium().colors.success());
        assert_eq!(t.colors.bg(), Theme::titanium().colors.bg());
    }

    #[test]
    fn empty_string_is_terminal_default() {
        let t = Theme::parse(r#"{"name":"t","colors":{"text":""}}"#).unwrap();
        assert_eq!(t.colors.text(), Color::Default);
    }

    #[test]
    fn var_chain_resolves() {
        let json =
            r##"{"name":"t","vars":{"a":"#111111","b":"$a","c":"$b"},"colors":{"accent":"$c"}}"##;
        let t = Theme::parse(json).unwrap();
        assert_eq!(t.colors.accent(), Color::Rgb(0x11, 0x11, 0x11));
    }

    #[test]
    fn var_cycle_is_loud() {
        let json = r#"{"name":"t","vars":{"a":"$b","b":"$a"},"colors":{"accent":"$a"}}"#;
        let err = Theme::parse(json).unwrap_err();
        assert!(err.0.contains("cycle"), "error should mention cycle: {err}");
        assert!(
            err.0.contains("a -> b -> a"),
            "cycle path should be explicit: {err}"
        );
    }

    #[test]
    fn undefined_var_is_loud() {
        let json = r#"{"name":"t","colors":{"accent":"$nope"}}"#;
        let err = Theme::parse(json).unwrap_err();
        assert!(err.0.contains("undefined var"));
    }

    #[test]
    fn malformed_hex_is_loud() {
        for bad in ["#12345", "#gggggg", "#12"] {
            let json = format!(r#"{{"name":"t","colors":{{"accent":"{bad}"}}}}"#);
            assert!(Theme::parse(&json).is_err(), "{bad} should fail");
        }
    }

    #[test]
    fn multibyte_hex_errors_instead_of_panicking() {
        // Regression for CodeRabbit CR-2: "…" is 3 bytes but 1 char, so a
        // byte-length dispatch used to index past the end and panic. Custom
        // themes are user input — this must be a loud error, not an abort.
        let json = r##"{"name":"t","colors":{"accent":"#…"}}"##;
        let err = Theme::parse(json).unwrap_err();
        assert!(
            err.0.contains("expected 3 or 6"),
            "loud error expected: {err}"
        );
        // Same for an emoji-only "hex" value (4 bytes, 1 char).
        let json = r##"{"name":"t","colors":{"accent":"#😀"}}"##;
        assert!(Theme::parse(json).is_err());
    }

    #[test]
    fn out_of_range_index_is_loud() {
        let json = r#"{"name":"t","colors":{"dim":256}}"#;
        assert!(Theme::parse(json).is_err());
    }

    #[test]
    fn unknown_top_level_key_is_loud() {
        let json = r#"{"name":"t","colors":{},"bogus":1}"#;
        let err = Theme::parse(json).unwrap_err();
        assert!(err.0.contains("bogus"));
    }

    #[test]
    fn unknown_color_token_is_loud() {
        let json = r##"{"name":"t","colors":{"notAToken":"#fff"}}"##;
        let err = Theme::parse(json).unwrap_err();
        assert!(err.0.contains("notAToken"));
    }

    #[test]
    fn missing_name_is_loud() {
        assert!(Theme::parse(r#"{"colors":{}}"#).is_err());
    }

    #[test]
    fn bad_preset_is_loud() {
        let json = r#"{"name":"t","colors":{},"symbols":{"preset":"comic"}}"#;
        let err = Theme::parse(json).unwrap_err();
        assert!(err.0.contains("comic"));
    }

    #[test]
    fn ascii_preset_swaps_glyphs() {
        let t = Theme::parse(r#"{"name":"t","colors":{},"symbols":{"preset":"ascii"}}"#).unwrap();
        assert_eq!(t.symbols.border_tl.as_ref(), "+");
        assert_eq!(t.symbols.border_h.as_ref(), "-");
        assert_eq!(t.symbols.dot_online.as_ref(), "*");
        assert_eq!(t.symbols.progress_fill.as_ref(), "#");
    }

    #[test]
    fn symbol_override_wins_over_preset() {
        let t = Theme::parse(
            r#"{"name":"t","colors":{},"symbols":{"preset":"ascii","progressFill":"━"}}"#,
        )
        .unwrap();
        assert_eq!(t.symbols.border_tl.as_ref(), "+"); // preset still applies elsewhere
        assert_eq!(t.symbols.progress_fill.as_ref(), "━"); // override wins here
    }

    #[test]
    fn spinner_frames_override() {
        let t =
            Theme::parse(r#"{"name":"t","colors":{},"symbols":{"spinnerFrames":["a","b","c"]}}"#)
                .unwrap();
        assert_eq!(t.symbols.spinner_frames, vec!["a", "b", "c"]);
    }

    #[test]
    fn empty_spinner_frames_are_loud() {
        let json = r#"{"name":"t","colors":{},"symbols":{"spinnerFrames":[]}}"#;
        assert!(Theme::parse(json).is_err());
    }

    #[test]
    fn color_mode_detection_order() {
        assert_eq!(
            detect_color_mode_from(Some("truecolor"), None),
            ColorMode::Truecolor
        );
        assert_eq!(
            detect_color_mode_from(Some("24bit"), None),
            ColorMode::Truecolor
        );
        assert_eq!(
            detect_color_mode_from(Some("TrueColor"), None),
            ColorMode::Truecolor
        );
        assert_eq!(
            detect_color_mode_from(None, Some("abc123")),
            ColorMode::Truecolor
        );
        assert_eq!(detect_color_mode_from(None, Some("")), ColorMode::Color256);
        assert_eq!(detect_color_mode_from(None, None), ColorMode::Color256);
        assert_eq!(
            detect_color_mode_from(Some("256color"), None),
            ColorMode::Color256
        );
        assert_eq!(
            detect_color_mode_from(Some("truecolor"), Some("x")),
            ColorMode::Truecolor
        );
    }

    #[test]
    fn ansi_256_quantization_sanity() {
        // Pure black/white anchor to cube corners.
        assert_eq!(rgb_to_ansi_256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi_256(255, 255, 255), 231);
        // Mid gray falls on the grayscale ramp.
        let g = rgb_to_ansi_256(128, 128, 128);
        assert!(
            (232..=255).contains(&g),
            "gray should hit the ramp, got {g}"
        );
        // Pure red maps to the red cube anchor.
        assert_eq!(rgb_to_ansi_256(255, 0, 0), 196);
        // Ramp overflow regression (CodeRabbit CR-1): avg == 247 must map to
        // ramp index 255, never truncate 256 → 0 (black).
        assert_eq!(rgb_to_ansi_256(247, 247, 247), 255);
        assert_ne!(rgb_to_ansi_256(247, 247, 247), 0);
        // Index passes through untouched.
        assert_eq!(Color::Index(42).to_ansi_256(), 42);
    }

    #[test]
    fn ratatui_conversion() {
        assert_eq!(
            Color::Rgb(1, 2, 3).to_ratatui(),
            ratatui::style::Color::Rgb(1, 2, 3)
        );
        assert_eq!(
            Color::Index(9).to_ratatui(),
            ratatui::style::Color::Indexed(9)
        );
        assert_eq!(Color::Default.to_ratatui(), ratatui::style::Color::Reset);
    }

    #[test]
    fn custom_theme_load_from_dir() {
        let dir = std::env::temp_dir().join(format!("harbour-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("solarized.json"),
            r##"{"name":"solarized","colors":{"accent":"#268bd2"}}"##,
        )
        .unwrap();
        let t = Theme::load_custom(&dir, "solarized").unwrap();
        assert_eq!(t.name, "solarized");
        assert_eq!(t.colors.accent(), Color::Rgb(0x26, 0x8b, 0xd2));
        assert_eq!(t.colors.bg(), Theme::titanium().colors.bg()); // inherited
        assert!(Theme::load_custom(&dir, "nope").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
