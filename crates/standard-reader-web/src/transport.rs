//! The web [`Transport`]: a **synchronous** `XMLHttpRequest`.
//!
//! This runs on the worker thread (a Web Worker), where a synchronous XHR — including
//! `responseType = "arraybuffer"` for binary blobs — is permitted and blocks only that worker,
//! never the page's main thread. The core builds every URL and parses every response; this just
//! moves bytes.

use std::fmt;

use standard_core::atp::Transport;
use web_sys::{XmlHttpRequest, XmlHttpRequestResponseType};

#[derive(Debug)]
pub struct WebTransportError(String);

impl fmt::Display for WebTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "web transport: {}", self.0)
    }
}

impl std::error::Error for WebTransportError {}

fn js_err(ctx: &str, e: wasm_bindgen::JsValue) -> WebTransportError {
    WebTransportError(format!("{ctx}: {e:?}"))
}

pub struct WebTransport;

impl WebTransport {
    pub fn new() -> Self {
        Self
    }

    fn request(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<Vec<u8>, WebTransportError> {
        let xhr = XmlHttpRequest::new().map_err(|e| js_err("new XHR", e))?;
        // async = false → synchronous (legal in a Worker; blocks only this thread).
        xhr.open_with_async(method, url, false)
            .map_err(|e| js_err("open", e))?;
        xhr.set_response_type(XmlHttpRequestResponseType::Arraybuffer);
        if let Some(ct) = content_type {
            xhr.set_request_header("content-type", ct)
                .map_err(|e| js_err("set header", e))?;
        }
        match body {
            // Public core reads use GET; `post` remains available for future core-level writes.
            // Authenticated subscription writes use Atrium's separate OAuth XHR client. Bodies are
            // UTF-8 JSON, so a string send is correct.
            Some(b) => xhr
                .send_with_opt_str(Some(&String::from_utf8_lossy(b)))
                .map_err(|e| js_err("send", e))?,
            None => xhr.send().map_err(|e| js_err("send", e))?,
        }
        let status = xhr.status().map_err(|e| js_err("status", e))?;
        if !(200..300).contains(&status) {
            return Err(WebTransportError(format!("HTTP {status} for {url}")));
        }
        let resp = xhr.response().map_err(|e| js_err("response", e))?;
        let bytes = js_sys::Uint8Array::new(&resp);
        let mut out = vec![0u8; bytes.length() as usize];
        bytes.copy_to(&mut out);
        Ok(out)
    }
}

impl Transport for WebTransport {
    type Error = WebTransportError;

    fn get(&self, url: &str) -> Result<Vec<u8>, Self::Error> {
        self.request("GET", url, None, None)
    }

    fn post(&self, url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>, Self::Error> {
        self.request("POST", url, Some(content_type), Some(body))
    }
}
