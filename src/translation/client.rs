extern crate alloc;

use core::fmt::Write as _;

use defmt::{error, info};
use embedded_io_async::{Read, Write};

const GOOGLE_API_KEY: &str = env!("GOOGLE_API_KEY");

/// Maximum JSON request body length.
const MAX_BODY_LEN: usize = 512;

/// Errors that can occur during translation.
#[derive(Debug, defmt::Format)]
pub enum TranslateError {
    /// The request body exceeded the internal buffer size.
    BodyTooLong,
    /// The HTTP request failed.
    RequestFailed,
    /// The response body could not be read.
    ResponseReadFailed,
    /// Could not find `translatedText` in the response JSON.
    ParseFailed,
}

/// Translate `text` from `source_lang` to `target_lang` using Google Translate
/// v2. Returns the number of bytes of translated text written into `rx_buf`.
pub async fn translate_text<S>(
    session: &mut S,
    text: &str,
    source_lang: &str,
    target_lang: &str,
    rx_buf: &mut [u8],
) -> Result<usize, TranslateError>
where
    S: Read + Write,
{
    // -- Build JSON body --
    let mut body_buf = [0u8; MAX_BODY_LEN];
    let body_len = {
        let mut w = WriteBuf::new(&mut body_buf);
        write!(w, "{{\"q\":\"").map_err(|_| TranslateError::BodyTooLong)?;
        write_json_escaped(&mut w, text)?;
        write!(
            w,
            "\",\"source\":\"{}\",\"target\":\"{}\",\"format\":\"text\"}}",
            source_lang, target_lang,
        )
        .map_err(|_| TranslateError::BodyTooLong)?;
        w.pos
    };
    let body = &body_buf[..body_len];

    info!(
        "Translating: \"{}\" ({} -> {})",
        text, source_lang, target_lang
    );

    // -- Build and send the HTTP request --
    let mut req_buf = [0u8; 1024];
    let req_len = {
        let mut w = WriteBuf::new(&mut req_buf);
        write!(
            w,
            "POST /language/translate/v2?key={} HTTP/1.1\r\n\
             Host: translation.googleapis.com\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            GOOGLE_API_KEY, body_len,
        )
        .map_err(|_| TranslateError::RequestFailed)?;
        w.pos
    };

    info!("Sending HTTP request ({} + {} bytes)...", req_len, body_len);

    session
        .write_all(&req_buf[..req_len])
        .await
        .map_err(|_| TranslateError::RequestFailed)?;
    session
        .write_all(body)
        .await
        .map_err(|_| TranslateError::RequestFailed)?;
    session
        .flush()
        .await
        .map_err(|_| TranslateError::RequestFailed)?;

    // -- Read HTTP response --
    let mut resp_buf = [0u8; 2048];
    let mut resp_len = 0usize;

    loop {
        if resp_len >= resp_buf.len() {
            error!("Response too large for buffer");
            return Err(TranslateError::ResponseReadFailed);
        }
        match session.read(&mut resp_buf[resp_len..]).await {
            Ok(0) => break,
            Ok(n) => resp_len += n,
            Err(_) => {
                if resp_len > 0 {
                    break;
                }
                error!("Error reading HTTP response");
                return Err(TranslateError::ResponseReadFailed);
            }
        }
    }

    // -- Parse HTTP status line --
    let resp_str =
        core::str::from_utf8(&resp_buf[..resp_len]).map_err(|_| TranslateError::ParseFailed)?;

    let header_end = crate::networking::find_header_end(&resp_buf[..resp_len]).ok_or_else(|| {
        error!("Could not find end of HTTP headers");
        TranslateError::ResponseReadFailed
    })?;

    let status_line_end = resp_str[..header_end].find("\r\n").unwrap_or(header_end);
    let status_line = &resp_str[..status_line_end];
    info!("HTTP response: {}", status_line);

    // Body starts after \r\n\r\n
    let body_start = header_end + 4;
    let raw_body = &resp_str[body_start..];

    let body_str = strip_chunked_framing(raw_body);
    info!("Response body ({} bytes): {}", body_str.len(), body_str);

    // -- Parse translatedText from JSON --
    let needle = "\"translatedText\"";
    if let Some(key_start) = body_str.find(needle) {
        let after_key = key_start + needle.len();
        let rest = &body_str[after_key..];
        if let Some(quote_pos) = rest.find('"') {
            let value_start = after_key + quote_pos + 1;
            if let Some(end) = body_str[value_start..].find('"') {
                let translated = &body_str[value_start..value_start + end];
                info!("Translated text: \"{}\"", translated);

                let translated_bytes = translated.as_bytes();
                let len = translated_bytes.len().min(rx_buf.len());
                rx_buf[..len].copy_from_slice(&translated_bytes[..len]);
                return Ok(len);
            }
        }
    }

    error!("Could not parse translatedText from response");
    Err(TranslateError::ParseFailed)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip HTTP chunked transfer encoding framing from a response body.
fn strip_chunked_framing(body: &str) -> &str {
    let trimmed = body.trim_start();
    if let Some(first_crlf) = trimmed.find("\r\n") {
        let size_str = trimmed[..first_crlf].trim();
        if !size_str.is_empty() && size_str.chars().all(|c| c.is_ascii_hexdigit()) {
            let data_start = first_crlf + 2;
            let rest = &trimmed[data_start..];
            if let Some(chunk_end) = rest.rfind("\r\n0\r\n") {
                return &rest[..chunk_end];
            }
            return rest.trim_end();
        }
    }
    body
}

/// A tiny `core::fmt::Write` adapter over a `&mut [u8]` buffer.
struct WriteBuf<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> WriteBuf<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }
}

impl core::fmt::Write for WriteBuf<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        if self.pos + bytes.len() > self.buf.len() {
            return Err(core::fmt::Error);
        }
        self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
        Ok(())
    }
}

/// Write `s` into `w`, escaping characters that are special in JSON strings.
fn write_json_escaped(w: &mut WriteBuf<'_>, s: &str) -> Result<(), TranslateError> {
    for ch in s.chars() {
        let res = match ch {
            '"' => w.write_str("\\\""),
            '\\' => w.write_str("\\\\"),
            '\n' => w.write_str("\\n"),
            '\r' => w.write_str("\\r"),
            '\t' => w.write_str("\\t"),
            c => w.write_char(c),
        };
        res.map_err(|_| TranslateError::BodyTooLong)?;
    }
    Ok(())
}
