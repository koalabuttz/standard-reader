//! `sr` — the terminal frontend for `standard-core`.
//!
//! This crate owns everything platform-specific: the `ratatui` UI, a `reqwest`
//! [`Transport`](standard_core::atp::Transport), a `redb`
//! [`Store`](standard_core::store::Store), OAuth, and the `RichDoc` renderer. The
//! engine lives in `standard-core`; a future Vita frontend swaps this crate out.

fn main() {
    println!("standard-reader (sr) — a TUI reader for standard.site");
    println!("scaffold up; the engine lives in `standard-core`. Frontend next.");
}
