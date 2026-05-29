//! User preferences: layout, colour theme, and per-blog overrides — persisted as a
//! human-editable `prefs.toml` in the config dir.
//!
//! This is durable *config*, not re-fetchable cache, so it lives beside the OAuth session files
//! (under `$XDG_CONFIG_HOME/standard-reader/`), not in the `redb` cache. The UI thread owns the
//! canonical [`Prefs`] (loaded at startup, mutated on user action); writes go through the worker
//! (`ToWorker::SavePrefs`) so `App` stays I/O-free.
//!
//! Loading is tolerant: a missing or malformed file → [`Prefs::default`]; missing fields fall
//! back per-field (`#[serde(default)]`), so a hand-edited file never aborts startup.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ui::theme::ThemeColors;

/// How the reader arranges its panes. Cycleable at runtime (`\`) and persisted.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayoutKind {
    /// One pane, full width — the focused pane only (feeds / posts / reader).
    OnePane,
    /// The classic two-column: feeds *or* posts on the left, reader on the right.
    #[default]
    TwoPane,
    /// Feeds | posts | reader, all visible at once.
    ThreePane,
    /// A collapsing drill-down: feeds (full) → feeds + posts → just the open post.
    /// (`alias` keeps `prefs.toml` files written before the rename from "feed-first" working.)
    #[serde(alias = "feed-first")]
    DrillDown,
}

impl LayoutKind {
    pub const ALL: [LayoutKind; 4] = [
        LayoutKind::OnePane,
        LayoutKind::TwoPane,
        LayoutKind::ThreePane,
        LayoutKind::DrillDown,
    ];

    /// The next layout in the cycle (wraps).
    pub fn next(self) -> LayoutKind {
        let i = Self::ALL.iter().position(|&l| l == self).unwrap_or(1);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    pub fn label(self) -> &'static str {
        match self {
            LayoutKind::OnePane => "One pane",
            LayoutKind::TwoPane => "Two pane",
            LayoutKind::ThreePane => "Three pane",
            LayoutKind::DrillDown => "Drill-down",
        }
    }
}

/// A per-publication override of layout and/or theme (`None` field = use the global default).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BlogOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<LayoutKind>,
}

impl BlogOverride {
    /// Whether this override carries nothing (so it can be dropped from the map).
    pub fn is_empty(&self) -> bool {
        self.theme.is_none() && self.layout.is_none()
    }
}

/// All persisted preferences. Field order matters for TOML: scalar values must precede the
/// table-valued fields (`custom`, `per_blog`), or the serializer rejects the document.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The chosen theme: a [`PRESETS`](crate::ui::theme::PRESETS) name, or `"custom"`.
    pub theme: String,
    pub layout: LayoutKind,
    /// Width of the feeds (sidebar) column.
    pub sidebar_width: u16,
    /// Width of the posts column (independent of the sidebar, in the multi-pane layouts).
    pub posts_width: u16,
    pub onboarded: bool,
    /// The editable custom palette (used when `theme == "custom"`).
    pub custom: ThemeColors,
    /// Per-publication overrides, keyed by the publication AT-URI.
    pub per_blog: HashMap<String, BlogOverride>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: "modern-dark".into(),
            layout: LayoutKind::TwoPane,
            sidebar_width: 30,
            posts_width: 36,
            onboarded: false,
            custom: ThemeColors::modern_dark(),
            per_blog: HashMap::new(),
        }
    }
}

/// Bounds for a resizable pane's width (clamped on every set + defensively at draw time).
pub const PANE_MIN: u16 = 16;
pub const PANE_MAX: u16 = 60;

impl Prefs {
    /// Load preferences from `path`, falling back to defaults on any missing/malformed file.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Serialize to TOML (pretty). Infallible from the caller's view — a serialize error yields
    /// an empty string rather than panicking (it can't happen for this shape).
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    /// Defaults but already onboarded — for tests that exercise the running app (not first launch).
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self {
            onboarded: true,
            ..Self::default()
        }
    }

    /// The override for a publication, if any.
    pub fn blog(&self, pub_uri: &str) -> Option<&BlogOverride> {
        self.per_blog.get(pub_uri)
    }

    /// Set (or, when the closure leaves it empty, clear) a publication's override.
    pub fn edit_blog(&mut self, pub_uri: &str, f: impl FnOnce(&mut BlogOverride)) {
        let mut ov = self.per_blog.get(pub_uri).cloned().unwrap_or_default();
        f(&mut ov);
        if ov.is_empty() {
            self.per_blog.remove(pub_uri);
        } else {
            self.per_blog.insert(pub_uri.to_string(), ov);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_round_trips_with_per_blog_overrides() {
        let mut p = Prefs {
            theme: "custom".into(),
            layout: LayoutKind::ThreePane,
            sidebar_width: 42,
            onboarded: true,
            ..Default::default()
        };
        p.edit_blog("at://did:plc:x/site.standard.publication/1", |o| {
            o.layout = Some(LayoutKind::OnePane);
            o.theme = Some("light".into());
        });
        let toml = p.to_toml();
        let back: Prefs = toml::from_str(&toml).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // A sparse hand-edited file: only the layout is set.
        let p: Prefs = toml::from_str("layout = \"three-pane\"").unwrap();
        assert_eq!(p.layout, LayoutKind::ThreePane);
        assert_eq!(p.theme, "modern-dark");
        assert_eq!(p.sidebar_width, 30);
        assert!(!p.onboarded);
    }

    #[test]
    fn pre_rename_feed_first_value_still_parses() {
        // `feed-first` was the old name for `drill-down`; the serde alias keeps old files working.
        let p: Prefs = toml::from_str("layout = \"feed-first\"").unwrap();
        assert_eq!(p.layout, LayoutKind::DrillDown);
    }

    #[test]
    fn garbage_file_yields_defaults() {
        // load() of a bad path / bad content → Default, never a panic.
        assert_eq!(
            Prefs::load(Path::new("/nonexistent/prefs.toml")),
            Prefs::default()
        );
    }

    #[test]
    fn layout_cycles_and_wraps() {
        assert_eq!(LayoutKind::OnePane.next(), LayoutKind::TwoPane);
        assert_eq!(LayoutKind::DrillDown.next(), LayoutKind::OnePane);
    }

    #[test]
    fn empty_override_is_dropped() {
        let mut p = Prefs::default();
        p.edit_blog("at://p/1", |o| o.theme = Some("light".into()));
        assert!(p.blog("at://p/1").is_some());
        p.edit_blog("at://p/1", |o| o.theme = None);
        assert!(
            p.blog("at://p/1").is_none(),
            "empty override removed from the map"
        );
    }
}
