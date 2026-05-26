//! `sr` — the terminal frontend for `standard-core`.
//!
//! This crate owns everything platform-specific: a `reqwest`
//! [`Transport`](standard_core::atp::Transport), and (soon) a `redb`
//! [`Store`](standard_core::store::Store), OAuth, and the `ratatui` UI + `RichDoc`
//! renderer. The engine lives in `standard-core`; a future Vita frontend swaps this out.
//!
//! For now it's a thin CLI demo of the read pipeline: `sr fetch <handle|did>`.

mod transport;

use std::error::Error;

use standard_core::atp::AtUri;
use standard_core::decode::Registry;
use standard_core::model::{Block, Inline, RichDoc};
use standard_core::read;

use transport::ReqwestTransport;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("fetch") => match args.get(1) {
            Some(target) => {
                if let Err(e) = run_fetch(target) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
            None => {
                eprintln!("usage: sr fetch <handle|did>");
                std::process::exit(2);
            }
        },
        _ => {
            println!("standard-reader (sr) — a TUI reader for standard.site");
            println!();
            println!("usage:");
            println!("  sr fetch <handle|did>   resolve a repo, list its docs, decode the first");
            println!();
            println!("the ratatui UI is next; this exercises the live read pipeline.");
        }
    }
}

/// Resolve a repo, walk subscriptions → publications → documents, and decode the first
/// document of each publication — the read pipeline, end to end on live data.
fn run_fetch(target: &str) -> Result<(), Box<dyn Error>> {
    let t = ReqwestTransport::new();
    let registry = Registry::with_defaults();

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
        println!("══ {} · {}", publication.name, publication.url);

        let (docs, cursor) = read::list_documents(&t, &repo, None)?;
        println!(
            "   {} document(s){}",
            docs.len(),
            if cursor.is_some() { " (more…)" } else { "" }
        );
        for doc in &docs {
            let title = if doc.title.is_empty() {
                "(untitled)"
            } else {
                &doc.title
            };
            println!("   • {title}  [{}]", doc.published_at);
        }

        if let Some(first) = docs.first() {
            let uri = AtUri::parse(&first.uri).ok_or("document has a malformed AT-URI")?;
            let (meta, body) = read::get_document(&t, &registry, &uri, &repo.pds)?;
            println!(
                "\n   ┌─ reading: {}",
                if meta.title.is_empty() {
                    "(untitled)"
                } else {
                    &meta.title
                }
            );
            print_doc(&body);
            println!("   └─");
        }
        println!();
    }
    Ok(())
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
