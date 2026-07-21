//! `standard-reader-web` — the browser/WASM shell (ratatui-in-the-browser via ratzilla).
//!
//! **Milestone 2 — OPFS persistence.** The platform-agnostic `standard-frontend` worker runs in a
//! Web Worker (`wasm_thread`), fetching over a synchronous-XHR [`WebTransport`] and caching in a
//! [`MemStore`] that now **persists to the Origin-Private File System** (see [`persist`]): the
//! cache survives reloads and reading works **offline, including images**. Still no auth (M3).
//!
//! The worker blocks on `recv()` and can't await, so async OPFS I/O lives on the **main thread**:
//! at startup we `await` the cache load *before* spawning the worker (so cache-first reads serve
//! offline), and the `draw_web` loop drains persist ops + writes them. `main` returns immediately;
//! the work runs inside a `spawn_local` so it can `await`.
//!
//! Threading needs cross-origin isolation (SharedArrayBuffer); see `Trunk.toml` (COOP/COEP) and the
//! nightly + build-std setup in `rust-toolchain.toml` / `.cargo/config.toml`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use ratzilla::ratatui::Terminal;
use ratzilla::{DomBackend, WebRenderer};

use standard_frontend::app::{App, PanelBorderStyle};
use standard_frontend::prefs::Prefs;
use standard_frontend::{input, ui, worker};

mod auth;
mod persist;
mod sink;
mod store;
mod transport;
use auth::NoAuth;
use persist::{BootstrapState, Opfs, WriteQueue};
use sink::OverlayImageSink;
use store::{MemStore, PersistOp};
use transport::WebTransport;

fn main() -> std::io::Result<()> {
    console_error_panic_hook::set_once();
    install_key_guard();

    // MemStore (in the worker) emits PersistOps; the main thread drains + writes them to OPFS.
    let (persist_tx, persist_rx) = channel::<PersistOp>();

    // The rest must `await` the OPFS cache load before building the store, so it runs on the main
    // thread's event loop. `main` returns at once; `draw_web` (registered inside) drives frames.
    wasm_bindgen_futures::spawn_local(async move {
        if let Err(e) = run(persist_tx, persist_rx).await {
            web_sys::console::log_1(&format!("fatal: {e}").into());
        }
    });

    Ok(())
}

/// The async bootstrap: open + hydrate the OPFS cache, spawn the worker over it, wire the reader.
async fn run(
    persist_tx: Sender<PersistOp>,
    persist_rx: Receiver<PersistOp>,
) -> std::io::Result<()> {
    let storage_notice: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    // Open OPFS + hydrate the cache. Best-effort: any failure → run in-memory only (never break).
    let (opfs, bootstrap) = match Opfs::open().await {
        Ok(o) => {
            let o = Rc::new(o);
            let state = persist::load_opfs(&o).await;
            (Some(o), state)
        }
        Err(e) => {
            web_sys::console::log_1(&format!("opfs unavailable, running in-memory: {e}").into());
            storage_notice.replace(Some(
                "offline storage unavailable; changes will not survive reload".into(),
            ));
            (None, BootstrapState::default())
        }
    };
    let completed_blobs = bootstrap.store.blobs.keys().cloned().collect::<Vec<_>>();

    // Spawn the worker in a Web Worker over the hydrated store. Preference saves cross the same
    // channel as cache writes so all OPFS work remains on the browser main thread.
    let log: Box<dyn Fn(&str) + Send> = Box::new(|msg| web_sys::console::log_1(&msg.into()));
    let prefs_tx = persist_tx.clone();
    let save_prefs: Box<dyn FnMut(&Prefs) + Send> = Box::new(move |prefs| {
        if let Ok(bytes) = serde_json::to_vec(prefs) {
            let _ = prefs_tx.send(PersistOp::Prefs(bytes));
        }
    });
    let (tx, rx) = worker::spawn(
        WebTransport::new(),
        MemStore::new(persist_tx, bootstrap.store),
        None::<NoAuth>,
        log,
        save_prefs,
    );

    let mut app = App::new(tx, bootstrap.prefs);
    app.set_open_url(Box::new(|url| {
        if let Some(win) = web_sys::window() {
            let _ = win.open_with_url(url);
        }
    }));
    // Rounded box-drawing corners rasterize as disconnected hooks in common browser monospace
    // fonts. Plain corners stay crisp in the DOM backend; the desktop shell keeps rounded corners.
    app.set_panel_border_style(PanelBorderStyle::Square);
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
    terminal.on_mouse_event({
        let app = app.clone();
        move |ev| {
            if let Some(mouse) = adapt_mouse(ev) {
                app.borrow_mut().on_mouse(mouse);
            }
        }
    });

    // Persist write-side state (main thread): one coalescing queue and at most one OPFS write at a
    // time. CIDs loaded at startup are already durable and do not need rewriting.
    let mut sink = OverlayImageSink::new();
    let write_queue = Rc::new(RefCell::new(WriteQueue::new(completed_blobs)));
    let writer_busy = Rc::new(Cell::new(false));

    terminal.draw_web(move |f| {
        // Drain worker results (non-blocking) before drawing.
        while let Ok(evt) = rx.try_recv() {
            app.borrow_mut().apply(evt);
        }
        // Drain persist ops → OPFS (best-effort). Without OPFS, still drain so the channel can't
        // grow unbounded.
        match &opfs {
            Some(opfs) => {
                while let Ok(op) = persist_rx.try_recv() {
                    write_queue.borrow_mut().push(op);
                }
                maybe_flush_writes(opfs, &write_queue, &writer_busy, &storage_notice);
            }
            None => while persist_rx.try_recv().is_ok() {},
        }
        if let Some(notice) = storage_notice.borrow_mut().take() {
            app.borrow_mut().status = format!("⚠ {notice}");
        }
        // Hide last frame's image overlays; `paint` re-shows the ones still on screen.
        sink.before_frame();
        ui::draw(f, &mut app.borrow_mut(), &mut sink);
    });

    Ok(())
}

/// Start the single OPFS writer when work is pending. Payload files are selected before snapshots,
/// every write gets one retry, and failure never stops the in-memory reader.
fn maybe_flush_writes(
    opfs: &Rc<Opfs>,
    queue: &Rc<RefCell<WriteQueue>>,
    busy: &Rc<Cell<bool>>,
    notice: &Rc<RefCell<Option<String>>>,
) {
    if busy.get() || queue.borrow().is_empty() {
        return;
    }
    busy.set(true);
    let opfs = opfs.clone();
    let queue = queue.clone();
    let busy = busy.clone();
    let notice = notice.clone();
    wasm_bindgen_futures::spawn_local(async move {
        while let Some(write) = { queue.borrow_mut().pop_next() } {
            let mut last_error = None;
            for _ in 0..2 {
                let (dir, name, bytes) = write.target();
                match opfs.write(dir, name, bytes).await {
                    Ok(()) => {
                        last_error = None;
                        break;
                    }
                    Err(e) => last_error = Some(e),
                }
            }
            if let Some(e) = last_error {
                web_sys::console::log_1(&format!("opfs {} write: {e}", write.label()).into());
                notice.replace(Some(
                    "offline storage write failed; recent changes may not survive reload".into(),
                ));
            } else {
                queue.borrow_mut().mark_succeeded(&write);
            }
        }
        busy.set(false);
    });
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
            "Tab"
                | "ArrowUp"
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
/// reader doesn't handle.
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

/// Map a browser left-button press into terminal-cell coordinates. Ratzilla reports viewport
/// pixels, while the shared frontend deliberately speaks in cells, so measure the live DOM grid
/// exactly as the native-image overlay does. This keeps clicks aligned through zoom and resize.
fn adapt_mouse(ev: ratzilla::event::MouseEvent) -> Option<input::MouseEvent> {
    use ratzilla::event::{MouseButton as RzButton, MouseEventKind as RzKind};
    if ev.event != RzKind::Pressed || ev.button != RzButton::Left {
        return None;
    }
    let (column, row) = dom_cell_at(ev.x as f64, ev.y as f64)?;
    let mut modifiers = input::KeyModifiers::NONE;
    if ev.ctrl {
        modifiers = modifiers | input::KeyModifiers::CONTROL;
    }
    if ev.shift {
        modifiers = modifiers | input::KeyModifiers::SHIFT;
    }
    if ev.alt {
        modifiers = modifiers | input::KeyModifiers::ALT;
    }
    Some(input::MouseEvent {
        kind: input::MouseEventKind::Down(input::MouseButton::Left),
        column,
        row,
        modifiers,
    })
}

fn dom_cell_at(client_x: f64, client_y: f64) -> Option<(u16, u16)> {
    let document = web_sys::window()?.document()?;
    let grid = document.get_element_by_id("grid")?;
    let first_row = grid.first_element_child()?;
    let first_cell = first_row.first_element_child()?;
    let cell_rect = first_cell.get_bounding_client_rect();
    let row_rect = first_row.get_bounding_client_rect();
    cell_from_geometry(
        client_x,
        client_y,
        cell_rect.left(),
        cell_rect.top(),
        cell_rect.width(),
        row_rect.height(),
        first_row.child_element_count(),
        grid.child_element_count(),
    )
}

#[allow(clippy::too_many_arguments)]
fn cell_from_geometry(
    client_x: f64,
    client_y: f64,
    origin_x: f64,
    origin_y: f64,
    cell_width: f64,
    cell_height: f64,
    columns: u32,
    rows: u32,
) -> Option<(u16, u16)> {
    if !client_x.is_finite()
        || !client_y.is_finite()
        || cell_width <= 0.0
        || cell_height <= 0.0
        || client_x < origin_x
        || client_y < origin_y
    {
        return None;
    }
    let column = ((client_x - origin_x) / cell_width).floor() as u32;
    let row = ((client_y - origin_y) / cell_height).floor() as u32;
    if column >= columns || row >= rows {
        return None;
    }
    Some((column.try_into().ok()?, row.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::cell_from_geometry;

    #[test]
    fn browser_pixels_map_to_terminal_cells_and_reject_outside_clicks() {
        let geom = |x, y| cell_from_geometry(x, y, 10.0, 20.0, 8.0, 15.0, 80, 24);
        assert_eq!(geom(10.0, 20.0), Some((0, 0)));
        assert_eq!(geom(33.9, 65.1), Some((2, 3)));
        assert_eq!(geom(9.9, 20.0), None);
        assert_eq!(geom(650.0, 20.0), None);
        assert_eq!(geom(10.0, 380.0), None);
    }
}
