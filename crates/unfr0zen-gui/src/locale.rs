//! Embedded GUI messages. English is the source catalog and fallback for translations.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;

fn english() -> &'static HashMap<String, String> {
    static CATALOG: OnceLock<HashMap<String, String>> = OnceLock::new();
    CATALOG.get_or_init(|| serde_json::from_str(include_str!("../locales/en.json")).expect("valid English GUI catalog"))
}

pub fn text(key: &str) -> &'static str {
    english().get(key).unwrap_or_else(|| panic!("missing GUI message: {key}")).as_str()
}

/// Substitutes named arguments without interpreting braces in their values.
pub fn format(key: &str, arguments: &[(&str, &dyn Display)]) -> String {
    render(text(key), arguments)
}

fn render(mut template: &str, arguments: &[(&str, &dyn Display)]) -> String {
    let mut output = String::new();
    while let Some((before, rest)) = template.split_once('{') {
        output.push_str(before);
        let (name, after) = rest.split_once('}').expect("closed GUI message placeholder");
        let value = arguments.iter().find(|(argument, _)| *argument == name)
            .unwrap_or_else(|| panic!("missing GUI message argument: {name}"));
        use std::fmt::Write;
        write!(output, "{}", value.1).expect("write GUI message to String");
        template = after;
    }
    output.push_str(template);
    output
}

/// Looks up a message, optionally substituting named display arguments.
#[macro_export]
macro_rules! tr {
    ($key:expr $(,)?) => { $crate::locale::text($key) };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::locale::format($key, &[$((stringify!($name), &$value as &dyn std::fmt::Display)),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_arguments_can_be_reordered_and_repeated() {
        assert_eq!(render("{count} {device}: {device}", &[("device", &"Nest"), ("count", &2)]), "2 Nest: Nest");
    }

    #[test]
    fn argument_values_are_not_interpreted_as_templates() {
        assert_eq!(render("File: {path}", &[("path", &"/{count}/image.bin")]), "File: /{count}/image.bin");
    }

    #[test]
    #[should_panic(expected = "missing GUI message argument: count")]
    fn missing_named_arguments_are_reported() {
        render("{count} devices", &[]);
    }

    #[test]
    fn every_gui_message_key_exists_in_the_catalog() {
        let sources = [include_str!("main.rs"), include_str!("patch.rs"), include_str!("terminal.rs"),
                       include_str!("about.rs"), include_str!("macos_about.rs"), include_str!("backups.rs"),
                       include_str!("bus.rs"), include_str!("nandmap.rs"), include_str!("setup.rs")];
        let mut used = std::collections::HashSet::new();
        for source in sources {
            for call in source.split("tr!(\"").skip(1) {
                let (key, _) = call.split_once('"').expect("a literal message key");
                assert!(english().contains_key(key), "missing GUI message: {key}");
                used.insert(key);
            }
        }
        for key in english().keys() {
            assert!(used.contains(key.as_str()), "unused GUI message: {key}");
        }
    }

    #[test]
    fn the_english_catalog_has_valid_messages_and_placeholders() {
        assert!(!english().is_empty());
        for (key, value) in english() {
            assert!(!key.is_empty() && !value.is_empty(), "{key}");
            let mut rest = value.as_str();
            let mut names = Vec::new();
            while let Some((before, after)) = rest.split_once('{') {
                assert!(!before.contains('}'), "{key}: unmatched closing brace");
                let (name, after) = after.split_once('}').expect("closed message placeholder");
                assert!(!name.is_empty() && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'), "{key}: {name}");
                names.push(name);
                rest = after;
            }
            assert!(!rest.contains('}'), "{key}: unmatched closing brace");
            let arguments: Vec<(&str, &dyn Display)> = names.iter().map(|&name| (name, &0 as &dyn Display)).collect();
            render(value, &arguments);
        }
    }
}
