use super::EntryKind;

pub struct SearchQuery {
    pub text: String,
    pub kind: Option<EntryKind>,
    pub extension: Option<String>,
}

pub fn parse_query(query: &str) -> SearchQuery {
    let mut text_parts = Vec::new();
    let mut kind = None;
    let mut extension = None;

    for part in query.split_whitespace() {
        if part.eq_ignore_ascii_case("#file") {
            kind = Some(EntryKind::File);
        } else if part.eq_ignore_ascii_case("#dir") {
            kind = Some(EntryKind::Directory);
        } else if let Some(ext) = part.strip_prefix("*.") {
            if !ext.is_empty() {
                extension = Some(ext.to_lowercase());
            } else {
                text_parts.push(part.to_lowercase());
            }
        } else {
            text_parts.push(part.to_lowercase());
        }
    }

    SearchQuery {
        text: text_parts.join(" "),
        kind,
        extension,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_kind_and_extension_filters() {
        let parsed = parse_query("Report *.TXT #FILE");
        assert_eq!(parsed.text, "report");
        assert_eq!(parsed.extension.as_deref(), Some("txt"));
        assert!(matches!(parsed.kind, Some(EntryKind::File)));
    }

    #[test]
    fn keeps_unknown_or_incomplete_filter_tokens_as_text() {
        let parsed = parse_query("#other *. notes");
        assert_eq!(parsed.text, "#other *. notes");
        assert!(parsed.kind.is_none());
        assert!(parsed.extension.is_none());
    }

    #[test]
    fn supports_directory_filter_and_multiple_words() {
        let parsed = parse_query("#dir Project Notes");
        assert_eq!(parsed.text, "project notes");
        assert!(matches!(parsed.kind, Some(EntryKind::Directory)));
        assert!(parsed.extension.is_none());
    }
}
