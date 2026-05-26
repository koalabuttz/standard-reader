//! `sr` — the terminal frontend for `standard-core`.
//!
//! This crate owns everything platform-specific: a `reqwest`
//! [`Transport`](standard_core::atp::Transport), a `redb`
//! [`Store`](standard_core::store::Store), the `ratatui` UI, and (soon) OAuth + images.
//! The engine lives in `standard-core`; a future Vita frontend swaps this crate out.
//!
//! `sr` with no args launches the interactive reader; `sr fetch`/`sr cached` are debug
//! CLI paths over the same pipeline.

mod app;
mod store;
mod transport;
mod ui;
mod worker;

use std::error::Error;
use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use ratatui_image::picker::Picker;

use standard_core::atp::AtUri;
use standard_core::decode::Registry;
use standard_core::model::{Block, Document, Inline, RichDoc};
use standard_core::read;
use standard_core::store::Store;

use app::App;
use store::RedbStore;
use transport::ReqwestTransport;
use ui::theme::Theme;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => run_tui(),
        Some("fetch") => match args.get(1) {
            Some(target) => run_fetch(target),
            None => {
                eprintln!("usage: sr fetch <handle|did>");
                std::process::exit(2);
            }
        },
        Some("cached") => run_cached(),
        Some("help" | "--help" | "-h") => {
            print_usage();
            Ok(())
        }
        Some(other) => {
            eprintln!("unknown command: {other}\n");
            print_usage();
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn print_usage() {
    println!("standard-reader (sr) — a TUI reader for standard.site");
    println!();
    println!("usage:");
    println!("  sr                      launch the interactive reader");
    println!("  sr fetch <handle|did>   (debug) fetch + decode + cache, print to stdout");
    println!("  sr cached               (debug) render the local cache, no network");
}

// --- the interactive reader -------------------------------------------------------

fn run_tui() -> Result<(), Box<dyn Error>> {
    let (tx, rx) = worker::spawn(cache_path()?);
    let mut terminal = ratatui::init();
    // Detect the terminal graphics protocol + font size (before mouse capture, so the
    // query's stdin replies aren't interleaved with mouse reports). Fall back to halfblocks.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    execute!(stdout(), EnableMouseCapture)?;

    let theme = Theme::modern_dark();
    let mut app = App::new(tx, picker);

    let outcome = (|| -> Result<(), Box<dyn Error>> {
        loop {
            terminal.draw(|f| ui::draw(f, &mut app, &theme))?;

            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                    Event::Mouse(m) => app.on_mouse(m),
                    _ => {}
                }
            }
            while let Ok(evt) = rx.try_recv() {
                app.apply(evt);
            }
            if app.should_quit {
                break;
            }
        }
        Ok(())
    })();

    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    outcome
}

// --- debug CLI paths --------------------------------------------------------------

/// Resolve a repo, walk subscriptions → publications → documents, decode them, and write
/// everything into the local `redb` cache as it goes.
fn run_fetch(target: &str) -> Result<(), Box<dyn Error>> {
    let t = ReqwestTransport::new();
    let registry = Registry::with_defaults();
    let mut cache = RedbStore::open(cache_path()?)?;

    let reader = read::resolve_identity(&t, target)?;
    println!("resolved {target}\n  did: {}\n  pds: {}\n", reader.did, reader.pds);

    let subs = read::list_subscriptions(&t, &reader)?;
    let publications: Vec<AtUri> = if subs.is_empty() {
        println!("no subscriptions — showing this repo's own publication(s):\n");
        read::list_publications(&t, &reader)?.iter().filter_map(|p| AtUri::parse(&p.uri)).collect()
    } else {
        println!("{} subscription(s):\n", subs.len());
        subs.iter().filter_map(|s| AtUri::parse(&s.publication)).collect()
    };

    for pub_uri in &publications {
        let (publication, repo) = read::get_publication(&t, pub_uri)?;
        cache.upsert_publication(&publication)?;
        println!("══ {} · {}\n   {}", publication.name, publication.url, publication.uri);

        let (repo_docs, cursor) = read::list_documents(&t, &repo, None)?;
        for doc in &repo_docs {
            cache.upsert_document(doc, None)?;
        }
        let docs: Vec<&Document> = repo_docs.iter().filter(|d| d.publication == publication.uri).collect();
        println!(
            "   {} document(s){} ({} in repo)",
            docs.len(),
            if cursor.is_some() { ", more…" } else { "" },
            repo_docs.len()
        );
        for doc in &docs {
            println!("   • {}  [{}]", title_or_untitled(&doc.title), doc.published_at);
        }

        if let Some(first) = docs.first() {
            let uri = AtUri::parse(&first.uri).ok_or("document has a malformed AT-URI")?;
            let (meta, body) = read::get_document(&t, &registry, &uri, &repo.pds)?;
            cache.upsert_document(&meta, Some(&body))?;
            println!("\n   ┌─ reading: {}", title_or_untitled(&meta.title));
            print_doc(&body);
            println!("   └─");
        }
        println!();
    }
    println!("cached → {}", cache_path()?.display());
    Ok(())
}

/// Render the local cache with no network — the offline-reading path.
fn run_cached() -> Result<(), Box<dyn Error>> {
    let cache = RedbStore::open(cache_path()?)?;
    let publications = cache.all_publications()?;
    if publications.is_empty() {
        println!("cache is empty — run `sr fetch <handle|did>` first.");
        return Ok(());
    }
    println!("cache: {}\n", cache_path()?.display());

    for publication in &publications {
        println!("══ {} · {}", publication.name, publication.url);
        let docs = cache.documents_for(&publication.uri)?;
        println!("   {} document(s)", docs.len());
        for doc in &docs {
            println!("   • {}  [{}]", title_or_untitled(&doc.title), doc.published_at);
        }
        if let Some(stored) = docs
            .iter()
            .find_map(|d| cache.document(&d.uri).ok().flatten().filter(|s| s.body.is_some()))
        {
            println!("\n   ┌─ reading: {}", title_or_untitled(&stored.meta.title));
            print_doc(stored.body.as_ref().unwrap());
            println!("   └─");
        }
        println!();
    }
    Ok(())
}

/// `$XDG_DATA_HOME` (or `~/.local/share`) + `standard-reader/cache.redb`.
fn cache_path() -> Result<PathBuf, Box<dyn Error>> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(std::env::var_os("HOME").ok_or("HOME not set")?).join(".local/share"),
    };
    let dir = base.join("standard-reader");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("cache.redb"))
}

fn title_or_untitled(title: &str) -> &str {
    if title.is_empty() { "(untitled)" } else { title }
}

// A minimal RichDoc → text renderer for the debug CLI (the TUI uses ui::doc).

fn print_doc(doc: &RichDoc) {
    for block in &doc.blocks {
        print_block(block, 1);
    }
}

fn print_block(block: &Block, indent: usize) {
    let pad = "   ".repeat(indent);
    match block {
        Block::Heading { level, content } => {
            println!("{pad}{} {}", "#".repeat(*level as usize), inlines(content));
        }
        Block::Paragraph(content) => println!("{pad}{}", inlines(content)),
        Block::Quote(blocks) => {
            for b in blocks {
                print!("{pad}│ ");
                print_block(b, 0);
            }
        }
        Block::List { ordered, items } => {
            for (i, item) in items.iter().enumerate() {
                let marker = if *ordered { format!("{}.", i + 1) } else { "•".to_string() };
                let text = item.iter().map(block_text).collect::<Vec<_>>().join(" ");
                println!("{pad}{marker} {text}");
            }
        }
        Block::Code { lang, text } => {
            println!("{pad}```{}", lang.as_deref().unwrap_or(""));
            for line in text.lines() {
                println!("{pad}{line}");
            }
            println!("{pad}```");
        }
        Block::Image(img) => println!("{pad}🖼  {} ({:?})", img.alt, img.source),
        Block::Rule => println!("{pad}───"),
    }
}

fn block_text(block: &Block) -> String {
    match block {
        Block::Paragraph(c) | Block::Heading { content: c, .. } => inlines(c),
        Block::Code { text, .. } => text.clone(),
        Block::Quote(bs) => bs.iter().map(block_text).collect::<Vec<_>>().join(" "),
        Block::List { items, .. } => {
            items.iter().flat_map(|i| i.iter().map(block_text)).collect::<Vec<_>>().join(" ")
        }
        Block::Image(img) => format!("[{}]", img.alt),
        Block::Rule => "───".to_string(),
    }
}

fn inlines(spans: &[Inline]) -> String {
    let mut out = String::new();
    for span in spans {
        match span {
            Inline::Text(t) => out.push_str(t),
            Inline::Strong(c) => out.push_str(&format!("**{}**", inlines(c))),
            Inline::Emphasis(c) => out.push_str(&format!("_{}_", inlines(c))),
            Inline::Strike(c) => out.push_str(&format!("~~{}~~", inlines(c))),
            Inline::Underline(c) => out.push_str(&format!("__{}__", inlines(c))),
            Inline::Code(t) => out.push_str(&format!("`{t}`")),
            Inline::Link { href, content } => out.push_str(&format!("[{}]({href})", inlines(content))),
            Inline::Image(img) => out.push_str(&format!("🖼 {}", img.alt)),
            Inline::LineBreak => out.push(' '),
        }
    }
    out
}
