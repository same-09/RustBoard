//! Small pure helpers for the Google Cloud voice: no network, no crates, easy to test.

/// Text as a JSON string literal, with the quotes.
pub fn json_string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// "id-ID-Standard-A" -> "id-ID". Google wants the language code next to the voice name.
pub fn lang_of(voice: &str) -> Option<String> {
    let mut p = voice.split('-');
    let (a, b) = (p.next()?, p.next()?);
    let ok = |s: &str, n: std::ops::RangeInclusive<usize>| n.contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric());
    (ok(a, 2..=3) && ok(b, 2..=4)).then(|| format!("{a}-{b}"))
}

/// Speed from the slider (-10..10) to Google's speaking rate (0.5 = half, 2.0 = double).
pub fn speaking_rate(rate: i32) -> f64 {
    2f64.powf(rate.clamp(-10, 10) as f64 / 10.0)
}

/// The JSON body for `text:synthesize`. Output is a WAV (LINEAR16).
pub fn synth_body(text: &str, voice: &str, rate: i32) -> Result<String, String> {
    let lang = lang_of(voice).ok_or("Pick a Google voice first.")?;
    Ok(format!(
        "{{\"input\":{{\"text\":{}}},\"voice\":{{\"languageCode\":{},\"name\":{}}},\"audioConfig\":{{\"audioEncoding\":\"LINEAR16\",\"speakingRate\":{:.3}}}}}",
        json_string(text),
        json_string(&lang),
        json_string(voice),
        speaking_rate(rate)
    ))
}

/// Standard base64 (what Google sends the audio as). Whitespace is skipped; bad input gives `None`.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                pad += 1;
                continue;
            }
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return None,
        };
        if pad > 0 {
            return None; // data after "="
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (pad <= 2).then_some(out)
}

/// A readable reason for a Google error answer (HTTP status + the message Google sent).
pub fn explain(status: u16, message: &str) -> String {
    let hint = match status {
        400 if message.to_ascii_lowercase().contains("api key") => " Check the API key.",
        400 => "",
        401 | 403 => " Check the API key, that Cloud Text-to-Speech API is turned on for it, and that billing is on.",
        429 => " Too many requests or the free quota is used up.",
        _ => "",
    };
    let msg = if message.is_empty() { "no details" } else { message };
    format!("Google said no ({status}): {msg}.{hint}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_text_is_escaped() {
        assert_eq!(json_string("a\"b\\c\nd\u{1}é"), "\"a\\\"b\\\\c\\nd\\u0001é\"");
    }

    #[test]
    fn language_comes_from_the_voice_name() {
        assert_eq!(lang_of("id-ID-Standard-A").as_deref(), Some("id-ID"));
        assert_eq!(lang_of("en-US-Chirp3-HD-Aoede").as_deref(), Some("en-US"));
        assert_eq!(lang_of("cmn-CN-Wavenet-A").as_deref(), Some("cmn-CN"));
        assert_eq!(lang_of(""), None);
        assert_eq!(lang_of("Microsoft David"), None);
        assert_eq!(lang_of("x"), None);
    }

    #[test]
    fn speed_maps_to_half_and_double() {
        assert!((speaking_rate(0) - 1.0).abs() < 1e-9);
        assert!((speaking_rate(-10) - 0.5).abs() < 1e-9);
        assert!((speaking_rate(10) - 2.0).abs() < 1e-9);
        assert!((speaking_rate(99) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn request_body_is_json_with_the_text_inside() {
        let b = synth_body("Halo \"dunia\"", "id-ID-Standard-A", 0).unwrap();
        assert!(b.starts_with("{\"input\":{\"text\":\"Halo \\\"dunia\\\"\"}"));
        assert!(b.contains("\"languageCode\":\"id-ID\""));
        assert!(b.contains("\"name\":\"id-ID-Standard-A\""));
        assert!(b.contains("\"audioEncoding\":\"LINEAR16\""));
        assert!(b.contains("\"speakingRate\":1.000"));
        assert!(synth_body("hi", "", 0).is_err());
    }

    #[test]
    fn base64_decodes() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("UklGRg==").unwrap(), b"RIFF");
        assert_eq!(base64_decode("UklGRg").unwrap(), b"RIFF");
        assert!(base64_decode("Zm9v!").is_none());
        assert!(base64_decode("Zg==Zg").is_none());
        assert!(base64_decode("Zg===").is_none());
    }

    #[test]
    fn errors_are_explained() {
        assert!(explain(403, "x").contains("billing"));
        assert!(explain(400, "API key not valid. Please pass a valid API key.").contains("Check the API key"));
        assert!(explain(429, "").contains("quota"));
        assert!(explain(500, "boom").starts_with("Google said no (500): boom."));
    }
}
