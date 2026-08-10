//! UI views (phase 2): search, downloads, status bar.
//!
//! Each view is a pure `draw(frame, area, state, theme)` function — no input
//! handling, no state mutation. The app loop (app.rs) owns keybind dispatch
//! and the 30fps tick; views just paint. All colors come from the theme's
//! curated subset (bg, text, accent, border, success, error, warning, muted,
//! dim, selected_bg).

pub mod downloads;
pub mod search;
pub mod status;
