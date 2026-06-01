//! `standard-reader-web` — the browser/WASM shell (ratatui-in-the-browser via ratzilla).
//!
//! **Milestone 1b — real worker + network.** The platform-agnostic `standard-frontend` worker runs
//! in a Web Worker (`wasm_thread`), fetching over a synchronous-XHR [`WebTransport`] and caching in
//! an in-memory [`MemStore`]. No auth, no persistence yet (M3 / M2). Press `a` to add a blog by
//! handle / DID / URL, then open a post — fetched live from its PDS.
//!
//! Threading needs cross-origin isolation (SharedArrayBuffer); see `Trunk.toml` (COOP/COEP) and the
//! nightly + build-std setup in `rust-toolchain.toml` / `.cargo/config.toml`.

use std::cell::RefCell;
use std::rc::Rc;

use ratzilla::ratatui::Terminal;
use ratzilla::{DomBackend, WebRenderer};

use standard_frontend::app::App;
use standard_frontend::prefs::Prefs;
use standard_frontend::{input, ui, worker};

mod auth;
mod sink;
mod store;
mod transport;
use auth::NoAuth;
use sink::OverlayImageSink;
use store::MemStore;
use transport::WebTransport;

fn main() -> std::io::Result<()> {
    console_error_panic_hook::set_once();
    install_key_guard();

    // Spawn the worker in a Web Worker. It logs to the browser console; M1b doesn't persist prefs.
    let log: Box<dyn Fn(&str) + Send> = Box::new(|msg| {
        web_sys::console::log_1(&msg.into());
    });
    let save_prefs: Box<dyn FnMut(&Prefs) + Send> = Box::new(|_prefs| {});
    let (tx, rx) = worker::spawn(
        WebTransport::new(),
        MemStore::new(),
        None::<NoAuth>,
        log,
        save_prefs,
    );

    let mut prefs = Prefs::default();
    prefs.onboarded = true; // skip the first-launch picker; land in the reader (press `a` to add)
    let mut app = App::new(tx, prefs);
    app.set_open_url(Box::new(|url| {
        if let Some(win) = web_sys::window() {
            let _ = win.open_with_url(url);
        }
    }));

    let app = Rc::new(RefCell::new(app));

    let backend = DomBackend::new()?;
    let terminal = Terminal::new(backend)?;

    terminal.on_key_event({
        let app = app.clone();
        move |ev| {
            if let Some(key) = adapt_key(ev) {
                app.borrow_mut().on_key(key);
            }
        }
    });

    let mut sink = OverlayImageSink::new();
    terminal.draw_web(move |f| {
        // Drain worker results (non-blocking) before drawing.
        while let Ok(evt) = rx.try_recv() {
            app.borrow_mut().apply(evt);
        }
        // Hide last frame's image overlays; `paint` re-shows the ones still on screen.
        sink.before_frame();
        ui::draw(f, &mut app.borrow_mut(), &mut sink);
    });

    Ok(())
}

/// Stop the browser from acting on the keys the reader drives — ratzilla's handler still fires
/// (so the app sees the key), but without this the browser *also* scrolls (arrows/space/page) or
/// moves focus (Tab) and pops its own shortcuts (Ctrl-P → print). We `preventDefault` exactly the
/// reader's navigation set, leaving Ctrl-R/C/V/etc. to the browser.
fn install_key_guard() {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    let Some(win) = web_sys::window() else {
        return;
    };
    let handler = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(|e: web_sys::KeyboardEvent| {
        let key = e.key();
        let nav = matches!(
            key.as_str(),
            "Tab" | "ArrowUp"
                | "ArrowDown"
                | "ArrowLeft"
                | "ArrowRight"
                | "PageUp"
                | "PageDown"
                | "Home"
                | "End"
                | " "
        );
        let ctrl_p = e.ctrl_key() && (key == "p" || key == "P");
        if nav || ctrl_p {
            e.prevent_default();
        }
    });
    let _ = win.add_event_listener_with_callback("keydown", handler.as_ref().unchecked_ref());
    handler.forget(); // keep the listener alive for the page's lifetime
}

/// Map a ratzilla key event to the frontend's neutral [`input::KeyEvent`]. `None` for keys the
/// reader doesn't handle. (Mouse is deferred: ratzilla's `MouseEvent` has no scroll and uses pixel
/// coordinates, so it needs cell-geometry + a wheel listener — a later step. Keyboard-only for now.)
fn adapt_key(ev: ratzilla::event::KeyEvent) -> Option<input::KeyEvent> {
    use ratzilla::event::KeyCode as Rz;
    let code = match ev.code {
        Rz::Char(c) => input::KeyCode::Char(c),
        Rz::Enter => input::KeyCode::Enter,
        Rz::Esc => input::KeyCode::Esc,
        Rz::Backspace => input::KeyCode::Backspace,
        Rz::Tab => input::KeyCode::Tab,
        Rz::Up => input::KeyCode::Up,
        Rz::Down => input::KeyCode::Down,
        Rz::Left => input::KeyCode::Left,
        Rz::Right => input::KeyCode::Right,
        Rz::PageUp => input::KeyCode::PageUp,
        Rz::PageDown => input::KeyCode::PageDown,
        _ => return None, // F(_), Delete, Home, End, Unidentified — unused by the reader
    };
    let mut mods = input::KeyModifiers::NONE;
    if ev.ctrl {
        mods = mods | input::KeyModifiers::CONTROL;
    }
    if ev.shift {
        mods = mods | input::KeyModifiers::SHIFT;
    }
    if ev.alt {
        mods = mods | input::KeyModifiers::ALT;
    }
    Some(input::KeyEvent::new(code, mods))
}
