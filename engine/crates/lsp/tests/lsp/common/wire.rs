use std::io::{self, BufRead, Write};
use std::path::Path;

use serde_json::{Value, json};

/// Build a URI the way the server does. `format!("file://{path}")` is only
/// valid where paths start with `/`; on Windows it yields `file://C:\a\b`,
/// which never matches the server's `file:///C:/a/b`.
pub fn path_uri(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    tower_lsp::lsp_types::Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

/// A canonical URI rewritten the way VS Code spells it: on Windows the drive
/// letter is lower-cased and its colon percent-encoded (`file:///d%3A/…`), which
/// is not what `Url::from_file_path` writes (#319). A no-op elsewhere, where the
/// two already agree.
pub fn client_spelling(uri: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(rest) = uri.strip_prefix("file:///")
            && let Some((drive, tail)) = rest.split_once(':')
            && drive.len() == 1
        {
            return format!("file:///{}%3A{tail}", drive.to_ascii_lowercase());
        }
    }
    uri.to_string()
}

pub fn jsonrpc_request(id: i64, method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
}

pub fn jsonrpc_notification(method: &str, params: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
    .to_string()
}

/// Build the `shutdown` request frame the server answers successfully.
///
/// `params` must be omitted entirely: tower-lsp 0.20 answers an explicit
/// `"params": null` shutdown with `-32602 Unexpected params: null`.
pub fn shutdown_request(id: i64) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "shutdown",
    })
    .to_string()
}

pub fn write_frame_to(stdin: &mut impl Write, body: &str) -> io::Result<()> {
    write!(stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
    stdin.flush()
}

/// Read one LSP frame and parse it, or `Err` once the server closes its stdout.
///
/// `read_line` reports EOF as `Ok(0)`, which has to surface as an error: a
/// dead server that reads as an endless run of empty frames made every wait
/// built on this time out blaming a missing notification (#198).
pub fn read_frame_from(reader: &mut impl BufRead) -> io::Result<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the server closed its stdout",
            ));
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            );
        }
    }
    let Some(length) = content_length else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a frame without a Content-Length header",
        ));
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}
