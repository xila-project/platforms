//! Framing of a `fetch` response as the raw HTTP/1.1 head the guests parse.
use alloc::{string::String, vec::Vec};
use file_system::{Error, Result};

/// Same limit as the native devices' response buffer.
pub(super) const MAX_HEAD_SIZE: usize = 8192;

/// Build `HTTP/1.1 <status> <reason>` followed by the headers and a blank line.
///
/// The browser has already removed any transfer or content coding, so the
/// body the guest reads is decoded. Headers that describe the encoded form
/// are dropped (`Transfer-Encoding`, `Content-Encoding`, and a `Content-Length`
/// that would count the compressed bytes) instead of misframing the body.
/// Browsers expose only the headers CORS allows, so the head is usually short.
pub(super) fn build_head(
    status: u16,
    reason: &str,
    headers: &[(String, String)],
) -> Result<Vec<u8>> {
    let encoded = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-encoding"));
    let mut head = Vec::new();
    let push = |head: &mut Vec<u8>, parts: &[&[u8]]| -> Result<()> {
        let length: usize = parts.iter().map(|part| part.len()).sum::<usize>() + 2;
        if head.len() + length + 2 > MAX_HEAD_SIZE {
            return Err(Error::FileTooLarge);
        }
        for part in parts {
            head.extend_from_slice(part);
        }
        head.extend_from_slice(b"\r\n");
        Ok(())
    };
    let mut digits = [0u8; 5];
    let status_text = status_digits(status, &mut digits);
    if reason.is_empty() {
        push(&mut head, &[b"HTTP/1.1 ", status_text])?;
    } else {
        push(
            &mut head,
            &[b"HTTP/1.1 ", status_text, b" ", reason.as_bytes()],
        )?;
    }
    for (name, value) in headers {
        let dropped = name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("content-encoding")
            || (encoded && name.eq_ignore_ascii_case("content-length"));
        if !dropped {
            push(&mut head, &[name.as_bytes(), b": ", value.as_bytes()])?;
        }
    }
    push(&mut head, &[])?;
    Ok(head)
}

fn status_digits(mut status: u16, digits: &mut [u8; 5]) -> &[u8] {
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (status % 10) as u8;
        status /= 10;
        if status == 0 {
            return &digits[start..];
        }
    }
}
