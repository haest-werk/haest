use haest::mods::ApprovalRequest;

use super::*;

fn ask(preview: &str) -> ApprovalRequest {
    ApprovalRequest::new("Bash", "haest wants to run a command that deletes files")
        .with_input_preview(preview)
        .with_key(preview)
}

/// Nothing in a command can draw over it on the terminal: no carriage
/// return, escape sequence or bidi override reaches it raw, and the end
/// of a long command shows.
#[test]
fn the_question_shows_the_command_as_it_is() {
    let drawn_over = question(&ask(
        "rm -rf ./build; touch PWNED #\r\u{1b}[2K  rm -rf ./build",
    ));
    let reordered = question(&ask("ls \u{202e}dlihc\u{202c} \u{200b}x"));
    let padded = question(&ask(&format!(
        "rm -rf ./build{}; touch PWNED",
        " ".repeat(186)
    )));

    for q in [&drawn_over, &reordered] {
        let raw: Vec<char> = q
            .chars()
            .filter(|&c| crate::render::hides(c) || (c.is_control() && c != '\n'))
            .collect();
        assert!(raw.is_empty(), "{raw:?} in {q:?}");
    }
    assert!(drawn_over.contains("touch PWNED"), "{drawn_over}");
    assert!(padded.contains("; touch PWNED"), "{padded}");
}

/// The rows `text` takes on a terminal `cols` wide.
fn rows(text: &str, cols: usize) -> usize {
    text.split('\n')
        .map(|l| l.chars().count().div_ceil(cols).max(1))
        .sum()
}

/// Padding cannot push a command's first line, or what is asked, off the
/// screen: the whole question fits in a few rows, and asks last.
#[test]
fn padding_cannot_push_the_command_or_the_question_off_the_screen() {
    let blank_lines = format!("touch PWNED3{}rm -rf ./build", "\n".repeat(41));
    let blank_spaced = format!("touch PWNED4{}rm -rf ./build", "\n \t".repeat(41));
    let spaces = format!("touch PWNED5;{}rm -rf ./build", " ".repeat(3000));
    let tabs = format!("touch PWNED6;{}rm -rf ./build", " \t".repeat(1500));
    for command in [blank_lines, blank_spaced, spaces, tabs] {
        let q = question(&ask(&command));
        assert!(rows(&q, 120) <= 6, "{} rows: {q}", rows(&q, 120));
        assert!(q.contains("touch PWNED"), "{q}");
        assert!(q.contains("rm -rf ./build"), "{q}");
        let last: Vec<&str> = q.rsplit('\n').take(2).collect();
        assert!(last[1].starts_with("Allow Bash? "), "asked last: {q}");
    }
    // A space that is not one shows as its escape, so it draws no blank
    // space: what is asked says how much is above it.
    let q = question(&ask(&format!(
        "touch PWNED7;{}rm -rf ./build",
        " \u{a0}".repeat(1500)
    )));
    let asked = q.rsplit('\n').nth(1).unwrap();
    assert!(
        asked.ends_with("(1 line, 10527 characters above)"),
        "{asked}"
    );
    assert!(q.contains(r"\u{a0}"), "{q}");
}

/// A command too long to read at a glance says so where it is asked.
#[test]
fn the_question_says_how_much_command_is_above_it() {
    let q = question(&ask("cd ./build\nrm -rf ./out\nls"));
    assert!(q.contains("(3 lines above)"), "{q}");
    let q = question(&ask(&format!("echo {}", "ab".repeat(400))));
    assert!(q.contains("(1 line, 805 characters above)"), "{q}");
    let q = question(&ask("rm -rf ./build"));
    assert!(!q.contains("above"), "{q}");
}

/// A character that draws blank but is not whitespace shows as its
/// escape, so it cannot pad a command unseen.
#[test]
fn a_blank_drawing_character_shows_as_its_escape() {
    for pad in ['\u{2800}', '\u{3164}', '\u{ffa0}', '\u{115f}'] {
        let command = format!(
            "touch PWNED6 #{}\nrm -rf ./build",
            pad.to_string().repeat(3000)
        );
        let q = question(&ask(&command));
        assert!(!q.contains(pad), "{pad:?} raw in the question");
        assert!(q.contains(&format!("{}", pad.escape_unicode())), "{q}");
    }
}

/// A tab shows as the word break bash makes of it, never as `\t`, which
/// bash would read as the letter `t`.
#[test]
fn a_tab_shows_as_the_word_break_it_is() {
    let q = question(&ask("rm -rf ./build\t~"));
    assert!(q.contains("./build⟨tab⟩~"), "{q}");
    assert!(!q.contains(r"\t"), "{q}");
}

/// A script's nesting reads as written; only indentation that could
/// push the rest off the screen is counted.
#[test]
fn a_scripts_indentation_reads_as_written() {
    let script = "for d in */; do\n    if [ -d \"$d/build\" ]; then\n        if [ -f \"$d/Makefile\" ]; then\n            rm -rf \"$d/build\"\n        fi\n    fi\ndone";
    let q = question(&ask(script));
    assert!(q.contains("\n              rm -rf \"$d/build\"\n"), "{q}");
    let q = question(&ask(&format!(
        "touch PWNED\n{}rm -rf ./build",
        " ".repeat(3000)
    )));
    assert!(q.contains("⟨3000 spaces⟩rm -rf ./build"), "{q}");
}
