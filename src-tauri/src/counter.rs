//! Counter text-file logic. Port of Counter.WriteToFile / ReadFromFile.
//!
//! Format uses the same "{0}" placeholder as the old C# app.
//! "{{" and "}}" are literal braces, like string.Format.

use crate::config::Counter;
use std::fs;
use std::path::Path;

/// Replace "{0}" with the count. Any other brace use is an error.
pub fn format_count(format: &str, count: i32) -> Result<String, String> {
    let mut out = String::new();
    let chars: Vec<char> = format.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' {
            if chars.get(i + 1) == Some(&'{') {
                out.push('{');
                i += 2;
            } else if chars.get(i + 1) == Some(&'0') && chars.get(i + 2) == Some(&'}') {
                out.push_str(&count.to_string());
                i += 3;
            } else {
                return Err(format!("Format '{}' is invalid.", format));
            }
        } else if c == '}' {
            if chars.get(i + 1) == Some(&'}') {
                out.push('}');
                i += 2;
            } else {
                return Err(format!("Format '{}' is invalid.", format));
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    Ok(out)
}

fn unescape(part: &str) -> String {
    part.replace("{{", "{").replace("}}", "}")
}

/// Pull the number back out of text written with `format`.
pub fn parse_count(format: &str, text: &str) -> Option<i32> {
    let (prefix, suffix) = format.split_once("{0}")?;
    let prefix = unescape(prefix);
    let suffix = unescape(suffix);
    let rest = text.strip_prefix(prefix.as_str())?;
    let digits = rest.strip_suffix(suffix.as_str())?;
    let body = digits.strip_prefix('-').unwrap_or(digits);
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse::<i32>().ok()
}

/// Write formatted count to the counter file. Creates the file if missing.
pub fn write_to_file(counter: &mut Counter) {
    counter.error = None;
    if counter.file.trim().is_empty() {
        return;
    }
    let formatted = match format_count(&counter.format, counter.count) {
        Ok(s) => s,
        Err(e) => {
            counter.error = Some(e);
            return;
        }
    };
    if let Err(e) = fs::write(Path::new(&counter.file), formatted) {
        counter.error = Some(format!("Cannot write file '{}': {}", counter.file, e));
    }
}

/// Read count from the counter file (if it matches the format).
pub fn read_from_file(counter: &mut Counter) {
    counter.error = None;
    if counter.file.trim().is_empty() {
        return;
    }
    let path = Path::new(&counter.file);
    if !path.exists() {
        counter.error = Some(format!("File '{}' does not exist.", counter.file));
        return;
    }
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            counter.error = Some(e.to_string());
            return;
        }
    };
    let content = content.trim_end_matches(&['\r', '\n'][..]);
    if content.is_empty() {
        return;
    }
    match parse_count(&counter.format, content) {
        Some(n) => counter.count = n,
        None => {
            counter.error = Some(format!(
                "Format '{}' does not match file {} content.",
                counter.format, counter.file
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_basic() {
        assert_eq!(format_count("Deaths: {0}", 5).unwrap(), "Deaths: 5");
        assert_eq!(format_count("{{{0}}}", 7).unwrap(), "{7}");
        assert!(format_count("{1}", 1).is_err());
        assert!(format_count("oops }", 1).is_err());
    }

    #[test]
    fn parse_basic() {
        assert_eq!(parse_count("Deaths: {0}", "Deaths: 12"), Some(12));
        assert_eq!(parse_count("{0} deaths", "-3 deaths"), Some(-3));
        assert_eq!(parse_count("{0}", "abc"), None);
        assert_eq!(parse_count("Deaths: {0}", "Kills: 1"), None);
    }
}
