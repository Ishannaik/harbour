mod anim;
mod app;
mod fake;
mod theme;
mod theme_watch;
mod types;
mod ui;

/// Loads the default titanium theme and hands control to the TUI. The theme
/// lives behind an `Arc<Mutex<Theme>>` for the process lifetime: the
/// theme-watcher thread swaps themes at runtime, so the render loop and every
/// view lock the same shared handle instead of caching a copy.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Color capability is detected once at startup, before the alt-screen
    // (docs/theming.md §Color mode detection) — fixed for the process lifetime.
    let _color_mode = theme::detect_color_mode();

    let theme = std::sync::Arc::new(std::sync::Mutex::new(theme::Theme::titanium()));
    app::run(theme)
}
