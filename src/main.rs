mod anim;
mod app;
mod fake;
mod queue_store;
mod theme;
mod theme_watch;
mod types;
mod ui;
mod watch;

/// Loads the default titanium theme and hands control to the TUI. The theme
/// lives behind an `Arc<Mutex<Theme>>` for the process lifetime: the
/// theme-watcher thread swaps themes at runtime, so the render loop and every
/// view lock the same shared handle instead of caching a copy.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Color capability is detected once at startup, before the alt-screen
    // (docs/theming.md §Color mode detection) — fixed for the process lifetime.
    let _color_mode = theme::detect_color_mode();

    let state_dir = queue_store::state_dir();
    let theme = std::sync::Arc::new(std::sync::Mutex::new(theme::Theme::titanium()));
    // Live theme reload (docs/theming.md §Custom themes): edits to the active
    // theme file under the themes dir swap in at the next render frame.
    theme_watch::spawn_theme_watcher(theme.clone());
    app::run(theme, &state_dir)
}
