# GUI message catalogs

`en.json` is the source catalog for the desktop app. Message IDs are stable,
context-specific keys; values contain the displayed English text. The app
currently uses English only.

Use `crate::tr!("message.id")` for plain text and
`crate::tr!("message.id", name = value)` for a message containing `{name}`.
Arguments are substituted by name, so translations can reorder them. Keep
sentences together rather than concatenating translated fragments.

To prepare a translation, copy `en.json` to a language-tagged JSON file and
translate its values, preserving the message IDs and named placeholders.
Additional catalogs need to be embedded and selected in `src/locale.rs`
before the app can display them. English remains the fallback catalog.

Do not translate internal widget IDs, commands, URLs, MQTT or other protocol
values, device identifiers, file paths, raw logs, or text supplied by a device.

Recovery stages, validation errors, and restore-confirmation details supplied
by the host library retain their English text. Their presentation needs typed
host messages before it can be translated independently of CLI output and logs.
Task names written to logs stay separate from translated GUI task names.
Dates continue to follow the operating system's regional formatting.
