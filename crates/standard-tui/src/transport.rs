//! The desktop [`Transport`]: synchronous HTTP via `reqwest::blocking`.
//!
//! This is one of the two seams a platform implements (the other is `Store`). The core
//! builds every URL and parses every response; this just moves bytes and will, once
//! OAuth lands, attach the auth header. [`TransportError`] keeps HTTP failures intact while
//! representing bounded-read and oversized-response failures explicitly.

use std::fmt;
use std::io::Read;
use std::time::Duration;

use standard_core::atp::Transport;

/// Maximum decompressed response retained from any PDS, discovery page, or image endpoint.
///
/// The core transport is deliberately generic, so this one ceiling covers both small XRPC JSON
/// and the largest legitimate payload class (publication images). Streaming through `take` also
/// bounds gzip/brotli expansion even when the advertised Content-Length is small.
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub enum TransportError {
    Http(reqwest::Error),
    Io(std::io::Error),
    TooLarge { limit: usize },
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(error) => write!(f, "{error}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::TooLarge { limit } => {
                write!(f, "response exceeded the {} MiB limit", limit / 1024 / 1024)
            }
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::TooLarge { .. } => None,
        }
    }
}

impl From<reqwest::Error> for TransportError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

pub struct ReqwestTransport {
    client: reqwest::blocking::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("standard-reader/", env!("CARGO_PKG_VERSION")))
            // Bound every fetch so a hung PDS can't freeze the (single-threaded) worker forever.
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");
        Self { client }
    }
}

impl Transport for ReqwestTransport {
    type Error = TransportError;

    fn get(&self, url: &str) -> Result<Vec<u8>, Self::Error> {
        let response = self.client.get(url).send()?.error_for_status()?;
        reject_announced_size(response.content_length(), MAX_RESPONSE_BYTES)?;
        read_bounded(response, MAX_RESPONSE_BYTES)
    }

    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>, Self::Error> {
        let response = self
            .client
            .post(url)
            .header("content-type", content_type)
            .body(body.to_vec())
            .send()?
            .error_for_status()?;
        reject_announced_size(response.content_length(), MAX_RESPONSE_BYTES)?;
        read_bounded(response, MAX_RESPONSE_BYTES)
    }
}

fn reject_announced_size(size: Option<u64>, limit: usize) -> Result<(), TransportError> {
    if size.is_some_and(|size| size > limit as u64) {
        return Err(TransportError::TooLarge { limit });
    }
    Ok(())
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, TransportError> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(TransportError::Io)?;
    if bytes.len() > limit {
        return Err(TransportError::TooLarge { limit });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bounded_reader_accepts_limit_and_rejects_one_byte_more() {
        assert_eq!(
            read_bounded(Cursor::new(b"1234"), 4).unwrap(),
            b"1234".to_vec()
        );
        assert!(matches!(
            read_bounded(Cursor::new(b"12345"), 4),
            Err(TransportError::TooLarge { limit: 4 })
        ));
        assert!(reject_announced_size(Some(4), 4).is_ok());
        assert!(reject_announced_size(Some(5), 4).is_err());
    }
}
