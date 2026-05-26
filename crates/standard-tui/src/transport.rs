//! The desktop [`Transport`]: synchronous HTTP via `reqwest::blocking`.
//!
//! This is one of the two seams a platform implements (the other is `Store`). The core
//! builds every URL and parses every response; this just moves bytes and will, once
//! OAuth lands, attach the auth header. `reqwest::Error` already satisfies the trait's
//! `Error: Error + Send + Sync + 'static` bound, so it is the associated error directly.

use standard_core::atp::Transport;

pub struct ReqwestTransport {
    client: reqwest::blocking::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("standard-reader/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("failed to build HTTP client");
        Self { client }
    }
}

impl Transport for ReqwestTransport {
    type Error = reqwest::Error;

    fn get(&self, url: &str) -> Result<Vec<u8>, Self::Error> {
        let bytes = self.client.get(url).send()?.error_for_status()?.bytes()?;
        Ok(bytes.to_vec())
    }

    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>, Self::Error> {
        let bytes = self
            .client
            .post(url)
            .header("content-type", content_type)
            .body(body.to_vec())
            .send()?
            .error_for_status()?
            .bytes()?;
        Ok(bytes.to_vec())
    }
}
