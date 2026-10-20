//! Attachment decoding and inspection helpers for the native host.
//!
//! Call `decode_attachment` from `spawn_blocking`; it does not touch egui or
//! host state. Unknown and malformed media remain inspectable as bytes rather
//! than turning a valid resource into an image-decoding error.

use base64::Engine as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodedAttachment {
    Image {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    Audio {
        mime: String,
        bytes: Vec<u8>,
    },
    Text {
        mime: String,
        text: String,
        truncated: bool,
        original_base64: String,
    },
    Binary {
        mime: String,
        byte_len: usize,
        hex_preview: String,
        decode_note: Option<String>,
        original_base64: String,
    },
}

const MAX_TEXT_PREVIEW_BYTES: usize = 1_048_576;
const MAX_BINARY_PREVIEW_BYTES: usize = 96;

/// Decode an inline base64 body or a data URL. The data URL's MIME type takes
/// precedence over the caller fallback so the host can preserve exact media
/// metadata from Codex and ACP content.
pub(crate) fn decode_inline(fallback_mime: &str, data: &str) -> Result<(String, Vec<u8>), String> {
    use base64::Engine;

    if let Some(rest) = data.strip_prefix("data:") {
        let (header, body) = rest
            .split_once(',')
            .ok_or_else(|| "Malformed media data URL".to_owned())?;
        let base64_encoded = header
            .split(';')
            .any(|part| part.eq_ignore_ascii_case("base64"));
        let mime = header
            .split(';')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or(fallback_mime)
            .to_owned();
        if !base64_encoded {
            return Ok((mime, percent_decode(body)));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body)
            .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(body))
            .map_err(|error| format!("Invalid base64 media data: {error}"))?;
        return Ok((mime, bytes));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(data))
        .map_err(|error| format!("Invalid base64 media data: {error}"))?;
    Ok((fallback_mime.to_owned(), bytes))
}

fn percent_decode(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push(((high << 4) | low) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    decoded
}

/// Classify and decode a loaded resource. Invoke away from the UI thread:
/// image decoding is CPU work, while text and unknown binary formats get a
/// compact preview suitable for the renderer.
pub(crate) fn decode_attachment(mime: &str, bytes: Vec<u8>) -> DecodedAttachment {
    let media_type = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    if media_type.starts_with("audio/") {
        return DecodedAttachment::Audio {
            mime: mime.to_owned(),
            bytes,
        };
    }

    if media_type.starts_with("image/") {
        let decoded = image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
            .with_guessed_format()
            .map_err(image::ImageError::IoError)
            .map(|mut reader| {
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(12_000);
                limits.max_image_height = Some(12_000);
                limits.max_alloc = Some(128 * 1024 * 1024);
                reader.limits(limits);
                reader.decode()
            })
            .and_then(|result| result);
        match decoded {
            Ok(image) => {
                let image = image.to_rgba8();
                return DecodedAttachment::Image {
                    width: image.width() as usize,
                    height: image.height() as usize,
                    rgba: image.into_raw(),
                };
            }
            Err(error) => {
                return binary_fallback(
                    mime,
                    &bytes,
                    Some(format!("Image decode failed: {error}")),
                );
            }
        }
    }

    if is_text_mime(mime) {
        let mut preview_end = bytes.len().min(MAX_TEXT_PREVIEW_BYTES);
        let text = match std::str::from_utf8(&bytes[..preview_end]) {
            Ok(text) => Some(text),
            Err(error) if error.error_len().is_none() => {
                preview_end = error.valid_up_to();
                std::str::from_utf8(&bytes[..preview_end]).ok()
            }
            Err(_) => None,
        };
        if let Some(text) = text {
            return DecodedAttachment::Text {
                mime: mime.to_owned(),
                text: text.to_owned(),
                truncated: bytes.len() > preview_end,
                original_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            };
        }
    }

    binary_fallback(mime, &bytes, None)
}

fn is_text_mime(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/ld+json"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/javascript"
                | "application/x-sh"
                | "application/toml"
                | "application/sql"
        )
}

fn binary_fallback(mime: &str, bytes: &[u8], decode_note: Option<String>) -> DecodedAttachment {
    DecodedAttachment::Binary {
        mime: mime.to_owned(),
        byte_len: bytes.len(),
        hex_preview: bytes
            .iter()
            .take(MAX_BINARY_PREVIEW_BYTES)
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" "),
        decode_note,
        original_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_codex_data_urls_and_raw_acp_base64() {
        assert_eq!(
            decode_inline(
                "application/octet-stream",
                "data:image/png;base64,Y2FyaWFk8J+QiQ=="
            )
            .unwrap(),
            ("image/png".into(), "cariad🐉".as_bytes().to_vec())
        );
        assert_eq!(
            decode_inline("audio/ogg", "4pyo").unwrap(),
            ("audio/ogg".into(), "✨".as_bytes().to_vec())
        );
        assert_eq!(
            decode_inline(
                "application/octet-stream",
                "data:text/plain;charset=utf-8,hello%20🐉"
            )
            .unwrap(),
            ("text/plain".into(), "hello 🐉".as_bytes().to_vec())
        );
    }

    #[test]
    fn unknown_pdf_and_malformed_images_have_inspectable_binary_fallbacks() {
        let pdf = decode_attachment("application/pdf", b"%PDF-1.7".to_vec());
        assert!(
            matches!(pdf, DecodedAttachment::Binary { mime, byte_len: 8, hex_preview, .. } if mime == "application/pdf" && hex_preview.starts_with("25 50 44 46"))
        );

        let broken_image = decode_attachment("image/png", b"not a png".to_vec());
        assert!(
            matches!(broken_image, DecodedAttachment::Binary { decode_note: Some(note), .. } if note.contains("Image decode failed"))
        );
    }

    #[test]
    fn audio_is_preserved_and_utf8_resources_are_readable() {
        assert_eq!(
            decode_attachment("audio/wav", vec![1, 2, 3]),
            DecodedAttachment::Audio {
                mime: "audio/wav".into(),
                bytes: vec![1, 2, 3]
            }
        );
        assert_eq!(
            decode_attachment("text/plain", "Cân 🐉".as_bytes().to_vec()),
            DecodedAttachment::Text {
                mime: "text/plain".into(),
                text: "Cân 🐉".into(),
                truncated: false,
                original_base64: base64::engine::general_purpose::STANDARD
                    .encode("Cân 🐉".as_bytes())
            }
        );
    }

    #[test]
    fn text_preview_is_bounded_without_losing_original_length() {
        let original = vec![b'x'; MAX_TEXT_PREVIEW_BYTES + 10];
        let decoded = decode_attachment("application/json", original.clone());
        match decoded {
            DecodedAttachment::Text {
                text,
                truncated,
                original_base64,
                ..
            } => {
                assert!(truncated);
                assert_eq!(text.len(), MAX_TEXT_PREVIEW_BYTES);
                assert_eq!(
                    base64::engine::general_purpose::STANDARD
                        .decode(original_base64)
                        .unwrap(),
                    original
                );
            }
            other => panic!("expected bounded text, got {other:?}"),
        }

        let mut boundary = vec![b'x'; MAX_TEXT_PREVIEW_BYTES - 1];
        boundary.extend_from_slice("🐉".as_bytes());
        assert!(
            matches!(decode_attachment("text/plain", boundary), DecodedAttachment::Text { text, truncated: true, .. } if text.len() == MAX_TEXT_PREVIEW_BYTES - 1)
        );
    }

    #[test]
    fn binary_preview_preserves_all_bytes_beyond_the_hex_window() {
        let original: Vec<u8> = (0..240).map(|byte| byte as u8).collect();
        let decoded = decode_attachment("application/pdf", original.clone());
        match decoded {
            DecodedAttachment::Binary {
                byte_len,
                hex_preview,
                original_base64,
                ..
            } => {
                assert_eq!(byte_len, original.len());
                assert_eq!(
                    hex_preview.split_whitespace().count(),
                    MAX_BINARY_PREVIEW_BYTES
                );
                assert_eq!(
                    base64::engine::general_purpose::STANDARD
                        .decode(original_base64)
                        .unwrap(),
                    original
                );
            }
            other => panic!("expected binary preview, got {other:?}"),
        }
    }
}
