//! Nothing haest writes to a terminal can act on it: [`inert`] escapes what
//! would, and the answer goes through it too unless stdout is a regular file.

use std::{borrow::Cow, io::IsTerminal};

use super::Renderer;

impl Renderer<std::io::Stdout, std::io::Stderr> {
    pub(crate) fn terminal(json: bool) -> Self {
        let color = std::io::stderr().is_terminal();
        let r = Renderer::new(std::io::stdout(), std::io::stderr(), json, color);
        // Byte for byte only into a file: a pipe can end on this terminal
        // (`| tee out.md`), right where a question is asked.
        if is_a_file(&std::io::stdout()) {
            r
        } else {
            r.inert_answer()
        }
    }
}

/// `out` is a regular file (`> out.md`), not a terminal, a pipe or a socket.
#[cfg(unix)]
fn is_a_file(out: &impl std::os::fd::AsFd) -> bool {
    out.as_fd()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .and_then(|f| f.metadata())
        .is_ok_and(|m| m.is_file())
}

/// `out` is a regular file (`> out.md`), not a console, a pipe or a socket.
#[cfg(windows)]
fn is_a_file(out: &impl std::os::windows::io::AsHandle) -> bool {
    out.as_handle()
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .and_then(|f| f.metadata())
        .is_ok_and(|m| m.is_file())
}

/// `text` with nothing in it a terminal acts on: every control character but
/// a newline and a tab, and every character that reorders or hides text, as
/// its escape (`\u{1b}`, `\r`, `\u{202e}`). An escape is printable, so a
/// second pass changes nothing.
pub(crate) fn inert(text: &str) -> Cow<'_, str> {
    let acts = |c: char| hides(c) || (c.is_control() && c != '\n' && c != '\t');
    if !text.chars().any(acts) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    for c in text.chars() {
        if hides(c) {
            out.extend(c.escape_unicode());
        } else if acts(c) {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Bidi controls and zero-width characters: they reorder text or hide it.
pub(crate) fn hides(c: char) -> bool {
    matches!(c,
        '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

#[cfg(test)]
mod tests {
    use haest::LoopEvent;
    use serde_json::json;

    use super::*;
    use crate::render::{
        Level,
        tests::{call, render, tool_result},
    };

    /// Nothing the model, a tool call's input or a notice carries reaches the
    /// terminal raw: no escape sequence or carriage return can redraw the
    /// screen around an approval question, and no bidi control can reorder it.
    #[test]
    fn nothing_from_the_model_or_a_tool_reaches_a_terminal_raw() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        {
            let mut r = Renderer::new(&mut out, &mut err, false, false).inert_answer();
            r.event(&LoopEvent::TextDelta {
                text: "I'll clean the build.\x1b[2J\x1b[H\x1b[2;3r\x1b[2H\n".into(),
            });
            let bash = call(
                "Bash",
                json!({"command": "true \x1b[2J\x1b[3;4r\x1b[1;1H\x1b[8m"}),
            );
            r.event(&tool_result(bash, false));
            r.notice(Level::Warning, "a server said\r\x1b[2KAllow Bash?");
            r.ask("Allow Bash? \u{202e}dlihc\u{202c}\n[y]es / [a]lways / [N]o: ");
        }
        for (name, bytes) in [("stdout", out), ("stderr", err.clone())] {
            let text = String::from_utf8(bytes).unwrap();
            let raw: Vec<char> = text
                .chars()
                .filter(|&c| hides(c) || (c.is_control() && c != '\n'))
                .collect();
            assert!(raw.is_empty(), "{name}: {raw:?} in {text:?}");
        }
        let err = String::from_utf8(err).unwrap();
        assert!(err.contains(r"- Bash true \u{1b}[2J"), "{err}");
        let once = inert("\x1b[2J\r\u{202e}").into_owned();
        assert_eq!(inert(&once), once, "a second pass changes nothing");
    }

    /// Into a file the answer goes byte for byte (`haest -p ... > out.md`).
    #[test]
    fn an_answer_into_a_file_keeps_its_bytes() {
        let text = "a\tb\x1b[1mc\u{202e}d\r\n";
        let (out, _) = render(false, &[LoopEvent::TextDelta { text: text.into() }]);
        assert_eq!(out, text);
    }

    /// The answer keeps its bytes only into a regular file: a pipe (`| tee
    /// out.md`, `| cat`) can end on the terminal where a question is asked.
    #[cfg(unix)]
    #[test]
    fn only_a_regular_file_takes_the_answer_byte_for_byte() {
        let file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
        assert!(is_a_file(&file));
        let (_reader, writer) = std::io::pipe().unwrap();
        assert!(!is_a_file(&writer), "a pipe can end on the terminal");
    }
}
