//! Google Cloud Translation API v2 client.
//!
//! Provides an async function to translate text using the Google Translate v2
//! REST API over HTTPS via raw HTTP over `mbedtls-rs` TLS sessions.
//!
//! Also includes a single-entry translation cache and a helper to extract
//! transcripts from Deepgram JSON responses and translate them.

extern crate alloc;

use alloc::string::String;
use core::cell::RefCell;
use core::fmt::Write as _;

use critical_section::Mutex;
use defmt::{error, info};
use embedded_io_async::{Read, Write};

/// Cached last translation: `(input_text, translated_text)`.
static LAST_TRANSLATION: Mutex<RefCell<Option<(String, String)>>> = Mutex::new(RefCell::new(None));

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

/// Find `\r\n\r\n` in a byte slice, returning the index of the first `\r`.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Translate `text` from `source_lang` to `target_lang` using Google Translate
/// v2. Returns the number of bytes of translated text written into `rx_buf`.
///
/// The caller is responsible for establishing the TLS session to
/// `translation.googleapis.com:443` before calling this function. The session
/// is passed in as a generic `S: Read + Write`, which allows using a
/// `mbedtls_rs::Session` wrapping a `TcpSocket`.
///
/// # Arguments
/// * `session` - A connected TLS session to `translation.googleapis.com:443`.
/// * `text` - The text to translate.
/// * `source_lang` - ISO-639 source language code (e.g. `"en"`).
/// * `target_lang` - ISO-639 target language code (e.g. `"es"`).
/// * `rx_buf` - Buffer where the translated text will be written.
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
    // We construct it manually to avoid pulling in serde.
    // The text content is escaped minimally (quotes and backslashes).
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
    // Construct the request line + headers + body into a stack buffer, then
    // write it all in one go over the TLS session.
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
                // A read error after we already have data may just mean the
                // server closed the connection (Connection: close). If we have
                // enough data, try to parse it.
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

    let header_end = find_header_end(&resp_buf[..resp_len]).ok_or_else(|| {
        error!("Could not find end of HTTP headers");
        TranslateError::ResponseReadFailed
    })?;

    let status_line_end = resp_str[..header_end].find("\r\n").unwrap_or(header_end);
    let status_line = &resp_str[..status_line_end];
    info!("HTTP response: {}", status_line);

    // Body starts after \r\n\r\n
    let body_start = header_end + 4;
    let raw_body = &resp_str[body_start..];

    // Handle chunked transfer encoding: if the body starts with a hex chunk
    // size followed by \r\n, strip the chunk framing to get the raw JSON.
    let body_str = strip_chunked_framing(raw_body);
    info!("Response body ({} bytes): {}", body_str.len(), body_str);

    // -- Parse translatedText from JSON --
    // Simple string search: find `"translatedText"` key and extract the value.
    // The server may include optional whitespace around the colon, so we search
    // for the key, skip `": ` or `":"`, then grab the quoted string.
    let needle = "\"translatedText\"";
    if let Some(key_start) = body_str.find(needle) {
        let after_key = key_start + needle.len();
        // Skip optional whitespace, colon, optional whitespace, opening quote.
        let rest = &body_str[after_key..];
        if let Some(quote_pos) = rest.find('"') {
            let value_start = after_key + quote_pos + 1;
            if let Some(end) = body_str[value_start..].find('"') {
                let translated = &body_str[value_start..value_start + end];
                info!("Translated text: \"{}\"", translated);

                // Copy the translated text into the caller's rx_buf so it can be
                // used after this function returns.
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
// Deepgram transcript extraction + cached translation
// ---------------------------------------------------------------------------

/// Extract the `"transcript"` value from a Deepgram JSON response.
///
/// Returns `None` if the field is missing, malformed, or empty.
pub fn extract_transcript(json: &str) -> Option<&str> {
    let needle = "\"transcript\":\"";
    let key_start = json.find(needle)?;
    let value_start = key_start + needle.len();
    let end = json[value_start..].find('"')?;
    let transcript = &json[value_start..value_start + end];

    if transcript.is_empty() {
        info!("Empty transcript, skipping translation");
        return None;
    }
    Some(transcript)
}

/// Check the single-entry translation cache. Returns the cached translation
/// if `transcript` matches the most recently translated input.
pub fn check_translation_cache(transcript: &str) -> Option<String> {
    critical_section::with(|cs| {
        let borrow = LAST_TRANSLATION.borrow_ref(cs);
        if let Some((ref prev_input, ref prev_result)) = *borrow {
            if prev_input == transcript {
                return Some(prev_result.clone());
            }
        }
        None
    })
}

/// Translate a Deepgram transcript from English to Spanish via Google
/// Translate, using the provided TLS session. Updates the translation cache
/// on success.
///
/// The caller is responsible for establishing the TLS session to
/// `translation.googleapis.com:443` and closing it afterwards.
pub async fn translate_response<S>(session: &mut S, transcript: &str)
where
    S: Read + Write,
{
    info!("Translating: \"{}\" (en -> es)...", transcript);

    let mut rx_buf = [0u8; 512];
    match translate_text(session, transcript, "en", "es", &mut rx_buf).await {
        Ok(len) => {
            let translated = core::str::from_utf8(&rx_buf[..len]).unwrap_or("<invalid UTF-8>");
            info!("Translation result: {}", translated);

            // Update cache.
            critical_section::with(|cs| {
                *LAST_TRANSLATION.borrow_ref_mut(cs) =
                    Some((String::from(transcript), String::from(translated)));
            });
        }
        Err(e) => {
            info!("Translation failed: {:?}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip HTTP chunked transfer encoding framing from a response body.
///
/// If the body looks like it starts with a hex chunk-size line (`<hex>\r\n`),
/// we extract just the chunk data (skipping size lines and the trailing
/// `0\r\n\r\n`). For non-chunked bodies this returns the input unchanged.
fn strip_chunked_framing(body: &str) -> &str {
    // A chunked body starts with hex digits followed by \r\n.
    // Quick heuristic: check if first non-whitespace chars are hex digits + \r\n.
    let trimmed = body.trim_start();
    if let Some(first_crlf) = trimmed.find("\r\n") {
        let size_str = trimmed[..first_crlf].trim();
        if !size_str.is_empty() && size_str.chars().all(|c| c.is_ascii_hexdigit()) {
            // Looks chunked. The actual data starts after the first \r\n.
            let data_start = first_crlf + 2;
            let rest = &trimmed[data_start..];
            // Find the end of this chunk (next \r\n before the next size line).
            // For our purposes, we just need the JSON, which ends at the next
            // `\r\n` before a `0\r\n` terminator or another chunk-size line.
            if let Some(chunk_end) = rest.rfind("\r\n0\r\n") {
                return &rest[..chunk_end];
            }
            // Fallback: strip trailing \r\n
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
