mod anim;
mod app;
mod theme;

/// Loads the default titanium theme and hands control to the TUI. The
/// engine/sources integration (async runtime, config loading) arrives in
/// later slices, so main stays deliberately minimal.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Color capability is detected once at startup, before the alt-screen
    // (docs/theming.md §Color mode detection): COLORTERM=truecolor|24bit, then
    // WT_SESSION, else 256. The 256-quantization path consumes this in the
    // slice-2 views; it is fixed for the process lifetime.
    let _color_mode = theme::detect_color_mode();

    let theme = theme::Theme::titanium();
    app::run(theme)
}
