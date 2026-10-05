//! Theme schema, validation, loading, live-reloading, and color mode detection.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ratatui::style::Color;
use serde::{Deserialize, Deserializer, Serialize};

/// Embedded default theme `titanium` (Tokyo Night palette).
pub const TITANIUM_JSON: &str = include_str!("../assets/themes/titanium.json");

/// Tokens in `colors` that every theme must specify.
pub const REQUIRED_TOKENS: &[&str] = &[
    "bg",
    "accent",
    "border",
    "success",
    "error",
    "warning",
    "muted",
    "dim",
    "text",
    "selectedBg",
    "statusLineBg",
];

/// Supported terminal color modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// 24-bit direct RGB color mode.
    TrueColor,
    /// Standard 256 ANSI palette color mode.
    TwoFiveSix,
}

/// Detect terminal color mode by inspecting environment variables via a closure.
///
/// Rules per docs/theming.md:
/// 1. `COLORTERM=truecolor` (case-insensitive; `24bit` also accepted) -> TrueColor.
/// 2. Else, `WT_SESSION` is set and non-empty -> TrueColor.
/// 3. Else -> TwoFiveSix.
pub fn detect_color_mode_with<F>(get_env: F) -> ColorMode
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(val) = get_env("COLORTERM") {
        let val = val.trim().to_lowercase();
        if val == "truecolor" || val == "24bit" {
            return ColorMode::TrueColor;
        }
    }
    if get_env("WT_SESSION").is_some_and(|val| !val.trim().is_empty()) {
        return ColorMode::TrueColor;
    }
    ColorMode::TwoFiveSix
}

/// Detect terminal color mode from the current process environment.
pub fn detect_color_mode() -> ColorMode {
    detect_color_mode_with(|k| std::env::var(k).ok())
}

/// Theme validation or loading errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeError {
    /// Invalid JSON syntax or structure.
    InvalidJson(String),
    /// Missing a required token in `colors`.
    MissingRequiredToken(String),
    /// Theme name was empty or missing.
    MissingName,
    /// Malformed hex color string.
    MalformedHex(String),
    /// ANSI 256 index out of range (0..=255).
    IndexOutOfRange(i64),
    /// Reference cycle detected in `vars`.
    VarCycle(String),
    /// Reference to an undefined variable in `vars`.
    UndefinedVar(String),
    /// Invalid color value format.
    InvalidColorValue(String),
    /// Invalid symbols preset name.
    InvalidPreset(String),
    /// File I/O error.
    Io(String),
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson(msg) => write!(f, "invalid theme JSON: {msg}"),
            Self::MissingRequiredToken(token) => write!(f, "missing required token: '{token}'"),
            Self::MissingName => write!(f, "theme name is required and cannot be empty"),
            Self::MalformedHex(hex) => write!(f, "malformed hex color: '{hex}'"),
            Self::IndexOutOfRange(idx) => {
                write!(f, "ANSI index {idx} out of range (must be 0..=255)")
            }
            Self::VarCycle(chain) => write!(f, "cycle detected in variable references: {chain}"),
            Self::UndefinedVar(var) => write!(f, "undefined variable reference: '${var}'"),
            Self::InvalidColorValue(val) => write!(f, "invalid color value: '{val}'"),
            Self::InvalidPreset(p) => write!(
                f,
                "invalid symbols preset '{p}' (expected 'unicode', 'nerd', or 'ascii')"
            ),
            Self::Io(msg) => write!(f, "theme I/O error: {msg}"),
        }
    }
}

impl std::error::Error for ThemeError {}

/// Raw color value as parsed from JSON.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RawColorValue {
    /// Integer index (0..=255).
    Index(i64),
    /// String form (hex, variable ref, or empty string).
    String(String),
}

impl<'de> Deserialize<'de> for RawColorValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde_json::Value;
        let v = Value::deserialize(deserializer)?;
        match v {
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(RawColorValue::Index(i))
                } else {
                    Err(serde::de::Error::custom(format!(
                        "numeric color value must be an integer, got {n}"
                    )))
                }
            }
            Value::String(s) => Ok(RawColorValue::String(s)),
            other => Err(serde::de::Error::custom(format!(
                "color value must be an integer or string, got {other:?}"
            ))),
        }
    }
}

/// Raw symbols table from theme JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSymbols {
    /// Glyph preset: "unicode", "nerd", or "ascii".
    pub preset: Option<String>,
    /// Animated spinner frame ring.
    pub spinner_frames: Option<Vec<String>>,
    // Per-key border and junction overrides
    pub border_tl: Option<String>,
    pub border_tr: Option<String>,
    pub border_bl: Option<String>,
    pub border_br: Option<String>,
    pub border_h: Option<String>,
    pub border_v: Option<String>,
    pub border_tee_d: Option<String>,
    pub border_tee_u: Option<String>,
    pub border_tee_l: Option<String>,
    pub border_tee_r: Option<String>,
    // Progress bar overrides
    pub progress_fill: Option<String>,
    pub progress_half: Option<String>,
    pub progress_empty: Option<String>,
    // Health dot overrides
    pub dot_online: Option<String>,
    pub dot_offline: Option<String>,
}

/// Top-level raw theme schema per docs/theming.md.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTheme {
    /// Unique theme identifier; custom themes must match filename `<name>.json`.
    pub name: String,
    /// Token to color-value mapping.
    pub colors: HashMap<String, RawColorValue>,
    /// Optional named variables referenceable via `$name`.
    #[serde(default)]
    pub vars: HashMap<String, RawColorValue>,
    /// Optional symbols table.
    #[serde(default)]
    pub symbols: Option<RawSymbols>,
    /// Optional export block (passed through verbatim, ignored by harbour).
    pub export: Option<serde_json::Value>,
}

/// Resolved color representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedColor {
    /// 24-bit RGB components.
    Rgb(u8, u8, u8),
    /// ANSI 256 index.
    Index(u8),
    /// Default / Reset (empty string "").
    Default,
}

impl ResolvedColor {
    /// Convert to ratatui `Color` based on the active color mode.
    pub fn to_ratatui_color(self, mode: ColorMode) -> Color {
        match self {
            Self::Default => Color::Reset,
            Self::Index(idx) => Color::Indexed(idx),
            Self::Rgb(r, g, b) => match mode {
                ColorMode::TrueColor => Color::Rgb(r, g, b),
                ColorMode::TwoFiveSix => Color::Indexed(rgb_to_ansi256(r, g, b)),
            },
        }
    }
}

/// Resolved symbol glyphs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSymbols {
    pub border_tl: String,
    pub border_tr: String,
    pub border_bl: String,
    pub border_br: String,
    pub border_h: String,
    pub border_v: String,
    pub border_tee_d: String,
    pub border_tee_u: String,
    pub border_tee_l: String,
    pub border_tee_r: String,
    pub progress_fill: String,
    pub progress_half: String,
    pub progress_empty: String,
    pub dot_online: String,
    pub dot_offline: String,
    pub spinner_frames: Vec<String>,
}

impl Default for ResolvedSymbols {
    fn default() -> Self {
        Self {
            border_tl: "╭".to_string(),
            border_tr: "╮".to_string(),
            border_bl: "╰".to_string(),
            border_br: "╯".to_string(),
            border_h: "─".to_string(),
            border_v: "│".to_string(),
            border_tee_d: "┬".to_string(),
            border_tee_u: "┴".to_string(),
            border_tee_l: "├".to_string(),
            border_tee_r: "┤".to_string(),
            progress_fill: "█".to_string(),
            progress_half: "▓".to_string(),
            progress_empty: "░".to_string(),
            dot_online: "●".to_string(),
            dot_offline: "○".to_string(),
            spinner_frames: vec![
                "⠋".into(),
                "⠙".into(),
                "⠹".into(),
                "⠸".into(),
                "⠼".into(),
                "⠴".into(),
                "⠦".into(),
                "⠧".into(),
                "⠇".into(),
                "⠏".into(),
            ],
        }
    }
}

/// Fully resolved and validated theme ready for UI rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTheme {
    /// Theme name.
    pub name: String,
    /// Active terminal color mode.
    pub mode: ColorMode,
    /// Resolved ratatui colors keyed by token name.
    pub colors: HashMap<String, Color>,
    /// Resolved symbols and spinner frames.
    pub symbols: ResolvedSymbols,
}

impl ResolvedTheme {
    /// Canvas background color (`bg`).
    pub fn bg(&self) -> Color {
        self.color("bg")
    }

    /// Primary accent color (`accent`).
    pub fn accent(&self) -> Color {
        self.color("accent")
    }

    /// Panel border color (`border`).
    pub fn border(&self) -> Color {
        self.color("border")
    }

    /// Success color (`success`).
    pub fn success(&self) -> Color {
        self.color("success")
    }

    /// Error color (`error`).
    pub fn error(&self) -> Color {
        self.color("error")
    }

    /// Warning color (`warning`).
    pub fn warning(&self) -> Color {
        self.color("warning")
    }

    /// Muted secondary text color (`muted`).
    pub fn muted(&self) -> Color {
        self.color("muted")
    }

    /// De-emphasized color (`dim`).
    pub fn dim(&self) -> Color {
        self.color("dim")
    }

    /// Primary text color (`text`).
    pub fn text(&self) -> Color {
        self.color("text")
    }

    /// Selected cursor row background (`selectedBg`).
    pub fn selected_bg(&self) -> Color {
        self.color("selectedBg")
    }

    /// Status line background color (`statusLineBg`).
    pub fn status_line_bg(&self) -> Color {
        self.color("statusLineBg")
    }

    /// Get color for any arbitrary token name, falling back to `Color::Reset`.
    pub fn color(&self, token: &str) -> Color {
        self.colors.get(token).copied().unwrap_or(Color::Reset)
    }
}

/// Parse a hex string `#RRGGBB` or `#RGB` into `(r, g, b)`.
pub fn parse_hex_color(hex: &str) -> Result<(u8, u8, u8), ThemeError> {
    if !hex.starts_with('#') {
        return Err(ThemeError::MalformedHex(hex.to_string()));
    }
    let body = &hex[1..];
    match body.len() {
        3 => {
            let r = u8::from_str_radix(&body[0..1], 16)
                .map_err(|_| ThemeError::MalformedHex(hex.to_string()))?;
            let g = u8::from_str_radix(&body[1..2], 16)
                .map_err(|_| ThemeError::MalformedHex(hex.to_string()))?;
            let b = u8::from_str_radix(&body[2..3], 16)
                .map_err(|_| ThemeError::MalformedHex(hex.to_string()))?;
            Ok((r * 17, g * 17, b * 17))
        }
        6 => {
            let r = u8::from_str_radix(&body[0..2], 16)
                .map_err(|_| ThemeError::MalformedHex(hex.to_string()))?;
            let g = u8::from_str_radix(&body[2..4], 16)
                .map_err(|_| ThemeError::MalformedHex(hex.to_string()))?;
            let b = u8::from_str_radix(&body[4..6], 16)
                .map_err(|_| ThemeError::MalformedHex(hex.to_string()))?;
            Ok((r, g, b))
        }
        _ => Err(ThemeError::MalformedHex(hex.to_string())),
    }
}

/// Resolve a raw color value recursively through `vars` with cycle detection.
fn resolve_raw_color(
    val: &RawColorValue,
    vars: &HashMap<String, RawColorValue>,
    stack: &mut Vec<String>,
) -> Result<ResolvedColor, ThemeError> {
    match val {
        RawColorValue::Index(idx) => {
            if !(0..=255).contains(idx) {
                return Err(ThemeError::IndexOutOfRange(*idx));
            }
            Ok(ResolvedColor::Index(*idx as u8))
        }
        RawColorValue::String(s) => {
            if s.is_empty() {
                Ok(ResolvedColor::Default)
            } else if let Some(var_name) = s.strip_prefix('$') {
                if let Some(pos) = stack.iter().position(|x| x == var_name) {
                    let mut cycle_vec = stack[pos..].to_vec();
                    cycle_vec.push(var_name.to_string());
                    let chain = cycle_vec
                        .iter()
                        .map(|v| format!("vars.{v}"))
                        .collect::<Vec<_>>()
                        .join(" -> ");
                    return Err(ThemeError::VarCycle(chain));
                }
                let target_val = vars
                    .get(var_name)
                    .ok_or_else(|| ThemeError::UndefinedVar(var_name.to_string()))?;
                stack.push(var_name.to_string());
                let result = resolve_raw_color(target_val, vars, stack);
                stack.pop();
                result
            } else if s.starts_with('#') {
                let (r, g, b) = parse_hex_color(s)?;
                Ok(ResolvedColor::Rgb(r, g, b))
            } else {
                Err(ThemeError::InvalidColorValue(s.clone()))
            }
        }
    }
}

/// Resolve raw symbols into `ResolvedSymbols` applying preset and per-key overrides.
fn resolve_symbols(raw: Option<&RawSymbols>) -> Result<ResolvedSymbols, ThemeError> {
    let mut resolved = ResolvedSymbols::default();
    let raw = match raw {
        Some(r) => r,
        None => return Ok(resolved),
    };

    if let Some(preset) = &raw.preset {
        match preset.as_str() {
            "unicode" => {}
            "ascii" => {
                resolved.border_tl = "+".into();
                resolved.border_tr = "+".into();
                resolved.border_bl = "+".into();
                resolved.border_br = "+".into();
                resolved.border_h = "-".into();
                resolved.border_v = "|".into();
                resolved.border_tee_d = "+".into();
                resolved.border_tee_u = "+".into();
                resolved.border_tee_l = "+".into();
                resolved.border_tee_r = "+".into();
                resolved.progress_fill = "#".into();
                resolved.progress_half = "=".into();
                resolved.progress_empty = ".".into();
                resolved.dot_online = "*".into();
                resolved.dot_offline = "o".into();
                resolved.spinner_frames = vec!["|".into(), "/".into(), "-".into(), "\\".into()];
            }
            "nerd" => {
                resolved.dot_online = "●".into();
                resolved.dot_offline = "○".into();
            }
            other => return Err(ThemeError::InvalidPreset(other.to_string())),
        }
    }

    if let Some(frames) = raw.spinner_frames.as_ref().filter(|f| !f.is_empty()) {
        resolved.spinner_frames = frames.clone();
    }

    macro_rules! apply_override {
        ($field:ident, $raw_field:ident) => {
            if let Some(v) = &raw.$raw_field {
                resolved.$field = v.clone();
            }
        };
    }

    apply_override!(border_tl, border_tl);
    apply_override!(border_tr, border_tr);
    apply_override!(border_bl, border_bl);
    apply_override!(border_br, border_br);
    apply_override!(border_h, border_h);
    apply_override!(border_v, border_v);
    apply_override!(border_tee_d, border_tee_d);
    apply_override!(border_tee_u, border_tee_u);
    apply_override!(border_tee_l, border_tee_l);
    apply_override!(border_tee_r, border_tee_r);
    apply_override!(progress_fill, progress_fill);
    apply_override!(progress_half, progress_half);
    apply_override!(progress_empty, progress_empty);
    apply_override!(dot_online, dot_online);
    apply_override!(dot_offline, dot_offline);

    Ok(resolved)
}

/// Validate and resolve a raw theme JSON string into `ResolvedTheme`.
///
/// Missing non-required tokens inherit from the embedded `titanium` theme.
pub fn parse_and_validate_theme(json: &str, mode: ColorMode) -> Result<ResolvedTheme, ThemeError> {
    let raw: RawTheme =
        serde_json::from_str(json).map_err(|e| ThemeError::InvalidJson(e.to_string()))?;

    if raw.name.trim().is_empty() {
        return Err(ThemeError::MissingName);
    }

    // Validate presence of required tokens in colors
    for req in REQUIRED_TOKENS {
        if !raw.colors.contains_key(*req) {
            return Err(ThemeError::MissingRequiredToken((*req).to_string()));
        }
    }

    // Validate that vars can be resolved and have no cycles (deterministic sort)
    let mut resolved_vars = HashMap::new();
    let mut var_keys: Vec<&String> = raw.vars.keys().collect();
    var_keys.sort();
    for k in var_keys {
        let v = &raw.vars[k];
        let mut stack = vec![k.clone()];
        let resolved = resolve_raw_color(v, &raw.vars, &mut stack)?;
        resolved_vars.insert(k.clone(), resolved);
    }

    // Resolve colors specified in theme
    let mut resolved_colors = HashMap::new();
    for (k, v) in &raw.colors {
        let mut stack = Vec::new();
        let color = resolve_raw_color(v, &raw.vars, &mut stack)?;
        resolved_colors.insert(k.clone(), color.to_ratatui_color(mode));
    }

    // Parse titanium fallback to inherit missing tokens
    let titanium = get_default_titanium(mode);
    for (k, v) in &titanium.colors {
        resolved_colors.entry(k.clone()).or_insert(*v);
    }

    let symbols = resolve_symbols(raw.symbols.as_ref())?;

    Ok(ResolvedTheme {
        name: raw.name,
        mode,
        colors: resolved_colors,
        symbols,
    })
}

/// Get the parsed default `titanium` theme.
pub fn get_default_titanium(mode: ColorMode) -> ResolvedTheme {
    let raw: RawTheme =
        serde_json::from_str(TITANIUM_JSON).expect("embedded titanium.json must be valid");
    let mut resolved_colors = HashMap::new();
    for (k, v) in &raw.colors {
        let mut stack = Vec::new();
        let color = resolve_raw_color(v, &raw.vars, &mut stack)
            .expect("titanium colors must resolve cleanly");
        resolved_colors.insert(k.clone(), color.to_ratatui_color(mode));
    }
    let symbols =
        resolve_symbols(raw.symbols.as_ref()).expect("titanium symbols must resolve cleanly");
    ResolvedTheme {
        name: raw.name,
        mode,
        colors: resolved_colors,
        symbols,
    }
}

/// Path resolution for `~/.harbour/` (Windows `%USERPROFILE%\.harbour`).
pub fn get_harbour_dir() -> PathBuf {
    if let Some(profile) = std::env::var("USERPROFILE")
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        return PathBuf::from(profile).join(".harbour");
    }
    if let Some(home) = std::env::var("HOME").ok().filter(|h| !h.trim().is_empty()) {
        return PathBuf::from(home).join(".harbour");
    }
    PathBuf::from(".harbour")
}

/// Resolve path to a theme file `~/.harbour/themes/<name>.json`.
pub fn get_theme_path(name: &str) -> PathBuf {
    get_harbour_dir()
        .join("themes")
        .join(format!("{name}.json"))
}

/// Load a theme from a file path with loud warning and fallback to default `titanium`.
pub fn load_theme_from_path(path: &Path, mode: ColorMode) -> (ResolvedTheme, Option<String>) {
    if !path.exists() {
        let msg = format!(
            "Theme file '{}' not found. Falling back to titanium.",
            path.display()
        );
        eprintln!("Warning: {msg}");
        return (get_default_titanium(mode), Some(msg));
    }

    match fs::read_to_string(path) {
        Ok(content) => match parse_and_validate_theme(&content, mode) {
            Ok(theme) => (theme, None),
            Err(err) => {
                let msg = format!(
                    "Theme '{}' failed validation ({}). Falling back to titanium.",
                    path.display(),
                    err
                );
                eprintln!("Warning: {msg}");
                (get_default_titanium(mode), Some(msg))
            }
        },
        Err(err) => {
            let msg = format!(
                "Failed to read theme file '{}': {}. Falling back to titanium.",
                path.display(),
                err
            );
            eprintln!("Warning: {msg}");
            (get_default_titanium(mode), Some(msg))
        }
    }
}

/// Load a theme by name with loud warning and fallback to default `titanium`.
///
/// Returns `(ResolvedTheme, Option<String>)` where the second element is an
/// optional warning message if fallback occurred.
pub fn load_theme_with_fallback(name: &str, mode: ColorMode) -> (ResolvedTheme, Option<String>) {
    if name == "titanium" {
        return (get_default_titanium(mode), None);
    }
    load_theme_from_path(&get_theme_path(name), mode)
}

/// Live-reloading theme file watcher that polls `mtime` without external crates.
pub struct ThemeWatcher {
    theme_name: String,
    path: PathBuf,
    last_mtime: Option<SystemTime>,
    poll_interval: Duration,
    accumulated: Duration,
}

impl ThemeWatcher {
    /// Create a new theme watcher for the specified theme name.
    pub fn new(theme_name: &str) -> Self {
        let path = get_theme_path(theme_name);
        let last_mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        Self {
            theme_name: theme_name.to_string(),
            path,
            last_mtime,
            poll_interval: Duration::from_secs(1),
            accumulated: Duration::ZERO,
        }
    }

    /// Create a watcher targeting a custom path (useful for testing).
    pub fn with_path(theme_name: &str, path: PathBuf, poll_interval: Duration) -> Self {
        let last_mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        Self {
            theme_name: theme_name.to_string(),
            path,
            last_mtime,
            poll_interval,
            accumulated: Duration::ZERO,
        }
    }

    /// Poll for file modification based on injected delta time `dt`.
    ///
    /// Returns:
    /// - `Some(Ok(new_theme))` on successful reload of a modified theme.
    /// - `Some(Err(error_msg))` when modified file failed validation (keeps current or falls back).
    /// - `None` if interval has not elapsed or file has not changed.
    pub fn poll(
        &mut self,
        dt: Duration,
        mode: ColorMode,
    ) -> Option<Result<ResolvedTheme, ThemeError>> {
        if self.theme_name == "titanium" {
            return None;
        }

        self.accumulated += dt;
        if self.accumulated < self.poll_interval {
            return None;
        }
        self.accumulated = Duration::ZERO;

        let current_mtime = match fs::metadata(&self.path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return None,
        };

        if Some(current_mtime) != self.last_mtime {
            self.last_mtime = Some(current_mtime);
            let content = match fs::read_to_string(&self.path) {
                Ok(c) => c,
                Err(e) => return Some(Err(ThemeError::Io(e.to_string()))),
            };
            Some(parse_and_validate_theme(&content, mode))
        } else {
            None
        }
    }
}

/// Quantize 24-bit RGB into nearest standard ANSI-256 color index.
///
/// Palette consists of:
/// - 0..15: Standard and high-intensity system colors
/// - 16..231: 6x6x6 color cube
/// - 232..255: Grayscale ramp
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    const CUBE_STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    const SYSTEM_16: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];

    let mut best_idx = 0u8;
    let mut min_dist = u32::MAX;

    // Check system 16
    for (i, &(sr, sg, sb)) in SYSTEM_16.iter().enumerate() {
        let dr = (r as i32) - (sr as i32);
        let dg = (g as i32) - (sg as i32);
        let db = (b as i32) - (sb as i32);
        let dist = (dr * dr + dg * dg + db * db) as u32;
        if dist < min_dist {
            min_dist = dist;
            best_idx = i as u8;
        }
    }

    // Check 6x6x6 cube (indices 16..=231)
    for (ri, &cr) in CUBE_STEPS.iter().enumerate() {
        for (gi, &cg) in CUBE_STEPS.iter().enumerate() {
            for (bi, &cb) in CUBE_STEPS.iter().enumerate() {
                let idx = 16 + (ri * 36 + gi * 6 + bi) as u8;
                let dr = (r as i32) - (cr as i32);
                let dg = (g as i32) - (cg as i32);
                let db = (b as i32) - (cb as i32);
                let dist = (dr * dr + dg * dg + db * db) as u32;
                if dist < min_dist {
                    min_dist = dist;
                    best_idx = idx;
                }
            }
        }
    }

    // Check grayscale ramp (indices 232..=255)
    for i in 0..24 {
        let val = 8 + 10 * i;
        let idx = 232 + i as u8;
        let dr = (r as i32) - val;
        let dg = (g as i32) - val;
        let db = (b as i32) - val;
        let dist = (dr * dr + dg * dg + db * db) as u32;
        if dist < min_dist {
            min_dist = dist;
            best_idx = idx;
        }
    }

    best_idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_titanium_embedded_is_valid() {
        let theme = parse_and_validate_theme(TITANIUM_JSON, ColorMode::TrueColor)
            .expect("embedded titanium theme must be valid");
        assert_eq!(theme.name, "titanium");
        assert_eq!(theme.bg(), Color::Rgb(0x16, 0x16, 0x1e));
        assert_eq!(theme.accent(), Color::Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(theme.dim(), Color::Indexed(240));
    }

    #[test]
    fn test_missing_required_token_fails() {
        let incomplete_json = r##"{
            "name": "incomplete",
            "colors": {
                "bg": "#000000",
                "accent": "#ffffff"
            }
        }"##;
        let err = parse_and_validate_theme(incomplete_json, ColorMode::TrueColor).unwrap_err();
        match err {
            ThemeError::MissingRequiredToken(t) => {
                assert!(REQUIRED_TOKENS.contains(&t.as_str()));
            }
            other => panic!("expected MissingRequiredToken, got {other:?}"),
        }
    }

    #[test]
    fn test_unknown_top_level_key_fails() {
        let bad_key_json = r##"{
            "name": "bad",
            "colors": {},
            "unsupportedKey": 123
        }"##;
        let err = parse_and_validate_theme(bad_key_json, ColorMode::TrueColor).unwrap_err();
        assert!(matches!(err, ThemeError::InvalidJson(_)));
    }

    #[test]
    fn test_var_cycle_detection() {
        let cycle_json = r##"{
            "name": "cycle",
            "colors": {
                "bg": "$a", "accent": "#ffffff", "border": "#ffffff",
                "success": "#ffffff", "error": "#ffffff", "warning": "#ffffff",
                "muted": "#ffffff", "dim": 240, "text": "#ffffff",
                "selectedBg": "#ffffff", "statusLineBg": "#ffffff"
            },
            "vars": {
                "a": "$b",
                "b": "$c",
                "c": "$a"
            }
        }"##;
        let err = parse_and_validate_theme(cycle_json, ColorMode::TrueColor).unwrap_err();
        match err {
            ThemeError::VarCycle(chain) => {
                assert!(chain.contains("vars.a -> vars.b -> vars.c -> vars.a"));
            }
            other => panic!("expected VarCycle, got {other:?}"),
        }
    }

    #[test]
    fn test_out_of_range_index_fails() {
        let bad_idx_json = r##"{
            "name": "bad_index",
            "colors": {
                "bg": 256, "accent": "#ffffff", "border": "#ffffff",
                "success": "#ffffff", "error": "#ffffff", "warning": "#ffffff",
                "muted": "#ffffff", "dim": 240, "text": "#ffffff",
                "selectedBg": "#ffffff", "statusLineBg": "#ffffff"
            }
        }"##;
        let err = parse_and_validate_theme(bad_idx_json, ColorMode::TrueColor).unwrap_err();
        assert_eq!(err, ThemeError::IndexOutOfRange(256));
    }

    #[test]
    fn test_malformed_hex_fails() {
        let bad_hex_json = r##"{
            "name": "bad_hex",
            "colors": {
                "bg": "#12", "accent": "#ffffff", "border": "#ffffff",
                "success": "#ffffff", "error": "#ffffff", "warning": "#ffffff",
                "muted": "#ffffff", "dim": 240, "text": "#ffffff",
                "selectedBg": "#ffffff", "statusLineBg": "#ffffff"
            }
        }"##;
        let err = parse_and_validate_theme(bad_hex_json, ColorMode::TrueColor).unwrap_err();
        assert_eq!(err, ThemeError::MalformedHex("#12".into()));
    }

    #[test]
    fn test_color_mode_detection() {
        // COLORTERM truecolor
        assert_eq!(
            detect_color_mode_with(|k| match k {
                "COLORTERM" => Some("truecolor".into()),
                _ => None,
            }),
            ColorMode::TrueColor
        );
        // COLORTERM 24bit case-insensitive
        assert_eq!(
            detect_color_mode_with(|k| match k {
                "COLORTERM" => Some("24Bit".into()),
                _ => None,
            }),
            ColorMode::TrueColor
        );
        // WT_SESSION set
        assert_eq!(
            detect_color_mode_with(|k| match k {
                "WT_SESSION" => Some("some-guid".into()),
                _ => None,
            }),
            ColorMode::TrueColor
        );
        // Default fallback to TwoFiveSix
        assert_eq!(detect_color_mode_with(|_| None), ColorMode::TwoFiveSix);
    }

    #[test]
    fn test_fallback_to_titanium_on_invalid_theme() {
        let (theme, warning) =
            load_theme_with_fallback("nonexistent-theme-xyz", ColorMode::TrueColor);
        assert_eq!(theme.name, "titanium");
        assert!(warning.is_some());
    }

    #[test]
    fn test_invalid_json_file_fallback_to_titanium() {
        let tmp_dir = std::env::temp_dir().join("harbour_corrupt_theme_test");
        let _ = fs::create_dir_all(&tmp_dir);
        let theme_file = tmp_dir.join("corrupt.json");
        fs::write(&theme_file, "{ not valid json").unwrap();

        let (theme, warning) = load_theme_from_path(&theme_file, ColorMode::TrueColor);
        assert_eq!(theme.name, "titanium");
        assert!(warning.is_some());
        assert!(warning.unwrap().contains("failed validation"));

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_theme_watcher_reload() {
        let tmp_dir = std::env::temp_dir().join("harbour_test_theme");
        let _ = fs::create_dir_all(&tmp_dir);
        let theme_file = tmp_dir.join("test_reload.json");

        let valid_v1 = r##"{
            "name": "test_reload",
            "colors": {
                "bg": "#111111", "accent": "#ffffff", "border": "#ffffff",
                "success": "#ffffff", "error": "#ffffff", "warning": "#ffffff",
                "muted": "#ffffff", "dim": 240, "text": "#ffffff",
                "selectedBg": "#ffffff", "statusLineBg": "#ffffff"
            }
        }"##;
        fs::write(&theme_file, valid_v1).unwrap();

        let mut watcher =
            ThemeWatcher::with_path("test_reload", theme_file.clone(), Duration::from_millis(10));

        // Before modification
        let res = watcher.poll(Duration::from_millis(20), ColorMode::TrueColor);
        assert!(res.is_none());

        // Modify file
        let valid_v2 = r##"{
            "name": "test_reload",
            "colors": {
                "bg": "#222222", "accent": "#ffffff", "border": "#ffffff",
                "success": "#ffffff", "error": "#ffffff", "warning": "#ffffff",
                "muted": "#ffffff", "dim": 240, "text": "#ffffff",
                "selectedBg": "#ffffff", "statusLineBg": "#ffffff"
            }
        }"##;
        // Ensure mtime ticks forward
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&theme_file, valid_v2).unwrap();

        let res = watcher.poll(Duration::from_millis(20), ColorMode::TrueColor);
        assert!(res.is_some());
        let new_theme = res.unwrap().expect("should parse successfully");
        assert_eq!(new_theme.bg(), Color::Rgb(0x22, 0x22, 0x22));

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}
