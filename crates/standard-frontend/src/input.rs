//! Platform-neutral input events.
//!
//! The reader's [`App`](crate::app::App) consumes these instead of `crossterm`'s event types.
//! `crossterm` is a terminal-specific crate that **doesn't compile to `wasm32`**, and the frontend
//! must stay platform-agnostic — so each shell adapts its native events into these: the desktop
//! maps `crossterm::event::*`, a browser shell maps ratzilla/DOM events. The shapes deliberately
//! mirror the small crossterm subset the reader uses (same names, `KeyEvent::new`,
//! `KeyModifiers::contains`), so the key/mouse handlers read identically.

/// A keyboard key. The subset of `crossterm::event::KeyCode` the reader handles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
}

/// Keyboard modifier flags — a `crossterm::event::KeyModifiers`-compatible bitset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(0b001);
    pub const CONTROL: Self = Self(0b010);
    pub const ALT: Self = Self(0b100);

    /// Whether all of `other`'s bits are set (mirrors `crossterm`'s `KeyModifiers::contains`).
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for KeyModifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// A key press. (No `kind` field: the reader only ever sees presses — each shell filters
/// key-release/repeat at its adapter.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// The mouse interactions the reader handles (a `crossterm::event::MouseEventKind` subset).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseEventKind {
    Down(MouseButton),
    ScrollUp,
    ScrollDown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MouseEvent {
    pub kind: MouseEventKind,
    pub column: u16,
    pub row: u16,
    pub modifiers: KeyModifiers,
}
