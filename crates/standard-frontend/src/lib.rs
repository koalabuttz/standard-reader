//! `standard-frontend` — the platform-agnostic frontend for `standard-reader`.
//!
//! Everything reusable across frontend shells lives here: the [`app::App`] state machine, the
//! `ratatui` [`ui`], the [`worker`] orchestration, [`prefs`]/theme, and the seam traits a platform
//! implements — [`frontend_store::FrontendStore`], [`auth_provider::AuthProvider`], and
//! [`image_sink::ImageSink`]. It depends on `standard-core` (the engine) plus `ratatui` + `image`,
//! but **not** on any platform stack (`reqwest`/`redb`/`tokio`/`atrium`/`ratatui-image`/`open`) —
//! those live in the per-platform shell (`standard-tui` for the desktop `sr` binary).

pub mod account;
pub mod app;
pub mod auth_provider;
pub mod frontend_store;
pub mod image_sink;
pub mod input;
pub mod prefs;
pub mod ui;
pub mod worker;
