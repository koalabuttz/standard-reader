//! `sr` — the terminal frontend for `standard-core`.
//!
//! This crate owns everything platform-specific: a `reqwest`
//! [`Transport`](standard_core::atp::Transport), a `redb`
//! [`Store`](standard_core::store::Store), and (soon) OAuth and the `ratatui` UI +
//! `RichDoc` renderer. The engine lives in `standard-core`; a future Vita frontend
//! swaps this crate out.
//!
//! For now it's a thin CLI: `sr fetch <handle|did>` (live, and caches), `sr cached`
//! (renders the cache with no network).

mod store;
mod transport;

use std::error::Error;
use std::path::PathBuf;

use standard_core::atp::AtUri;
use standard_core::decode::Registry;
use standard_core::model::{Block, Document, Inline, RichDoc};
use standard_core::read;
use standard_core::store::Store;

use store::RedbStore;
use transport::ReqwestTransport;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("fetch") => match args.get(1) {
            Some(target) => run_fetch(target),
            None => {
                eprintln!("usage: sr fetch <handle|did>");
                std::process::exit(2);
            }
        },
        Some("cached") => run_cached(),
        _ => {
            print_usage();
            return;
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
    println!("  sr fetch <handle|did>   resolve a repo, list its docs, decode + cache them");
    println!("  sr cached               render the local cache (no network)");
    println!();
    println!("the ratatui UI is next; these exercise the live pipeline and the offline cache.");
}

/// Resolve a repo, walk subscriptions → publications → documents, decode them, and write
/// everything into the local `redb` cache as it goes.
fn run_fetch(target: &str) -> Result<(), Box<dyn Error>> {
    let t = ReqwestTransport::new();
    let registry = Registry::with_defaults();
    let mut cache = RedbStore::open(cache_path()?)?;

    let reader = read::resolve_identity(&t, target)?;
    println!("resolved {target}");
    println!("  did: {}", reader.did);
    println!("  pds: {}\n", reader.pds);

    // Prefer the reader's subscriptions; fall back to the repo's own publications.
    let subs = read::list_subscriptions(&t, &reader)?;
    let publications: Vec<AtUri> = if subs.is_empty() {
        println!("no subscriptions — showing this repo's own publication(s):\n");
        read::list_publications(&t, &reader)?
            .iter()
            .filter_map(|p| AtUri::parse(&p.uri))
            .collect()
    } else {
        println!("{} subscription(s):\n", subs.len());
        subs.iter()
            .filter_map(|s| AtUri::parse(&s.publication))
            .collect()
    };

    for pub_uri in &publications {
        let (publication, repo) = read::get_publication(&t, pub_uri)?;
        cache.upsert_publication(&publication)?;
        println!("══ {} · {}", publication.name, publication.url);
        println!("   {}", publication.uri);

        // `listRecords` lists the whole repo, but a repo can host several publications;
        // a document belongs to the one its `site` field names. Cache every repo doc,
        // but show only this publication's.
        let (repo_docs, cursor) = read::list_documents(&t, &repo, None)?;
        for doc in &repo_docs {
            cache.upsert_document(doc, None)?; // metadata; body filled when read
        }
        let docs: Vec<&Document> = repo_docs
            .iter()
            .filter(|d| d.publication == publication.uri)
            .collect();
        println!(
            "   {} document(s){} ({} in repo)",
            docs.len(),
            if cursor.is_some() { ", more…" } else { "" },
            repo_docs.len()
        );
        for doc in &docs {
            println!(
                "   • {}  [{}]",
                title_or_untitled(&doc.title),
                doc.published_at
            );
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
            println!(
                "   • {}  [{}]",
                title_or_untitled(&doc.title),
                doc.published_at
            );
        }
        // Render the newest doc whose body was cached.
        if let Some(stored) = docs.iter().find_map(|d| {
            cache
                .document(&d.uri)
                .ok()
                .flatten()
                .filter(|s| s.body.is_some())
        }) {
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
    if title.is_empty() {
        "(untitled)"
    } else {
        title
    }
}

// --- a minimal RichDoc → text renderer (the real ratatui renderer comes later) ------

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
                let marker = if *ordered {
                    format!("{}.", i + 1)
                } else {
                    "•".to_string()
                };
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

/// Flatten a block to a single line of text (for list items).
fn block_text(block: &Block) -> String {
    match block {
        Block::Paragraph(c) | Block::Heading { content: c, .. } => inlines(c),
        Block::Code { text, .. } => text.clone(),
        Block::Quote(bs) => bs.iter().map(block_text).collect::<Vec<_>>().join(" "),
        Block::List { items, .. } => items
            .iter()
            .flat_map(|i| i.iter().map(block_text))
            .collect::<Vec<_>>()
            .join(" "),
        Block::Image(img) => format!("[{}]", img.alt),
        Block::Rule => "───".to_string(),
    }
}

/// Render inline spans to text with lightweight markers.
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
            Inline::Link { href, content } => {
                out.push_str(&format!("[{}]({href})", inlines(content)))
            }
            Inline::Image(img) => out.push_str(&format!("🖼 {}", img.alt)),
            Inline::LineBreak => out.push(' '),
        }
    }
    out
}
