//! Which web links the app may open in the browser (the UI asks the backend to open them).

/// Only our own repo and the VB-CABLE site. Nothing else, no spaces or quotes.
pub fn allowed(url: &str) -> bool {
    const PREFIXES: [&str; 2] = ["https://github.com/same-09/RustBoard/", "https://vb-audio.com/"];
    !url.chars().any(|c| c.is_whitespace() || c == '"' || c == '\'')
        && PREFIXES.iter().any(|p| url.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn our_links_pass() {
        assert!(allowed("https://github.com/same-09/RustBoard/releases/tag/v0.0.2"));
        assert!(allowed("https://vb-audio.com/Cable/"));
    }

    #[test]
    fn other_links_are_refused() {
        assert!(!allowed("https://evil.example/"));
        assert!(!allowed("https://github.com/same-09/RustBoardEvil/"));
        assert!(!allowed("https://github.com/other/RustBoard/"));
        assert!(!allowed("http://vb-audio.com/Cable/"));
        assert!(!allowed("file:///C:/Windows/System32/calc.exe"));
        assert!(!allowed("calc.exe"));
        assert!(!allowed(""));
    }

    #[test]
    fn no_tricks_inside_an_allowed_prefix() {
        assert!(!allowed("https://vb-audio.com/Cable/ --flag"));
        assert!(!allowed("https://vb-audio.com/Cable/\" & calc"));
    }
}
