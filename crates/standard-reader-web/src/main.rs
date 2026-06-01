//! `standard-reader-web` — the browser/WASM shell (ratatui-in-the-browser via ratzilla).
//!
//! **Milestone 1a — render spike.** Proves the platform-agnostic `standard-frontend` reader renders
//! and responds to the keyboard in a browser, driven by *canned* data — no worker, no network, no
//! persistence yet (those are M1b/M2/M3). The only wiring: ratzilla's `draw_web` render loop, a
//! `ratzilla → standard_frontend::input` key adapter, a stub image sink, and `App::set_open_url`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::channel;

use ratzilla::ratatui::Terminal;
use ratzilla::{DomBackend, WebRenderer};

use standard_core::model::{Block, Document, Inline, Publication, RichDoc};
use standard_frontend::app::App;
use standard_frontend::prefs::Prefs;
use standard_frontend::worker::{FromWorker, ToWorker};
use standard_frontend::{input, ui};

mod sink;
use sink::StubSink;

fn main() -> std::io::Result<()> {
    console_error_panic_hook::set_once();
    install_key_guard();

    // `App` requires a `ToWorker` sender; M1a runs no worker, so the receiver is dropped — `App`
    // ignores send errors, so its startup `LoadHome` simply goes nowhere.
    let (tx, _rx) = channel::<ToWorker>();
    let mut prefs = Prefs::default();
    prefs.onboarded = true; // skip the first-launch layout/theme picker for the spike
    let mut app = App::new(tx, prefs);
    app.set_open_url(Box::new(|url| {
        if let Some(win) = web_sys::window() {
            let _ = win.open_with_url(url);
        }
    }));

    // Canned content so there's something to render without a worker/network.
    feed_canned(&mut app);

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

    // The stub sink lives in the render closure (only the reader touches it).
    let mut sink = StubSink;
    terminal.draw_web(move |f| {
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
/// coordinates, so it needs cell-geometry + a wheel listener — a later step. M1a is keyboard-only.)
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

/// Push a feed + a post into `App` via the same `FromWorker` messages a real worker would send.
fn feed_canned(app: &mut App) {
    let pub_uri = "at://did:plc:demo/site.standard.publication/1";
    let doc_uri = "at://did:plc:demo/site.standard.document/1";

    app.apply(FromWorker::Feeds {
        feeds: vec![Publication {
            uri: pub_uri.into(),
            url: "https://demo.example".into(),
            name: "Demo Blog".into(),
            description: None,
            icon: None,
        }],
        unread: vec![(pub_uri.into(), 1)],
    });
    app.apply(FromWorker::Docs {
        publication: pub_uri.into(),
        docs: vec![Document {
            uri: doc_uri.into(),
            title: "Hello from the browser".into(),
            description: None,
            publication: pub_uri.into(),
            published_at: "2026-06-01T00:00:00Z".into(),
            updated_at: None,
            cover_image: None,
            text_content: Some("a canned post".into()),
            tags: vec![],
            path: None,
        }],
        read_uris: vec![],
        has_older: false,
    });
    app.apply(FromWorker::Doc {
        uri: doc_uri.into(),
        body: RichDoc {
            blocks: vec![
                Block::Heading {
                    level: 1,
                    content: vec![Inline::Text("Hello from the browser".into())],
                },
                Block::Paragraph(vec![
                    Inline::Text("This reader is ".into()),
                    Inline::Strong(vec![Inline::Text("ratatui".into())]),
                    Inline::Text(
                        " compiled to WebAssembly, rendered by ratzilla. Use ↑/↓ and Tab.".into(),
                    ),
                ]),
            ],
        },
        from_cache: true,
    });
}
