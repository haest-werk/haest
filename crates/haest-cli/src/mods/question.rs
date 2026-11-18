//! An approval asked on the terminal: the command as it runs, with nothing in
//! it that can hide part of it, then what is asked, last.

use haest::mods::ApprovalRequest;

use crate::render::inert;

/// A command longer than this many characters is counted where it is asked.
const LONG: usize = 500;

/// The command, then what is asked, last, right above the answer: the screen
/// always shows what is asked, and how much command is above it.
pub(super) fn question(req: &ApprovalRequest) -> String {
    let asked = format!(
        "Allow {}? {}",
        shown(&req.tool_name),
        shown(&req.description)
    );
    let answers = "[y]es / [a]lways / [N]o: ";
    let preview = req.input_preview.trim();
    if preview.is_empty() || preview == req.description.trim() {
        return format!("{asked}\n{answers}");
    }
    let command = visible(&squeezed(&inert(preview)));
    let (lines, chars) = (command.lines().count(), command.chars().count());
    let above = match (lines, chars > LONG) {
        (1, false) => String::new(),
        (1, true) => format!(" (1 line, {chars} characters above)"),
        (n, false) => format!(" ({n} lines above)"),
        (n, true) => format!(" ({n} lines, {chars} characters above)"),
    };
    let command = command.replace('\n', "\n  ");
    format!("  {command}\n{asked}{above}\n{answers}")
}

/// `text` as the terminal shows it, each further line indented under the
/// first.
fn shown(text: &str) -> String {
    visible(&inert(text)).replace('\n', "\n  ")
}

/// How a tab is shown: the word break bash makes of it, in notation that is
/// not shell syntax (`\t` would read as the letter `t`).
const TAB: &str = "⟨tab⟩";

/// `text`, [`inert`] already, with nothing in it that passes for a space or
/// draws as nothing: a tab as [`TAB`], and any other whitespace but a space
/// or a newline, and every [`blank`] character, as its escape.
fn visible(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str(TAB),
            c if (c.is_whitespace() && !c.is_ascii()) || blank(c) => {
                out.extend(c.escape_unicode());
            }
            c => out.push(c),
        }
    }
    out
}

/// Not whitespace, yet drawn as blank cells or as nothing: the Hangul
/// fillers, the blank braille pattern, and the default-ignorable code points
/// [`hides`](crate::render::inert::hides) leaves out.
fn blank(c: char) -> bool {
    matches!(c,
        '\u{00ad}' | '\u{034f}' | '\u{115f}' | '\u{1160}' | '\u{17b4}' | '\u{17b5}'
        | '\u{180b}'..='\u{180f}' | '\u{206a}'..='\u{206f}' | '\u{2800}' | '\u{3164}'
        | '\u{fe00}'..='\u{fe0f}' | '\u{ffa0}' | '\u{fff0}'..='\u{fff8}'
        | '\u{1bca0}'..='\u{1bca3}' | '\u{1d159}' | '\u{1d173}'..='\u{1d17a}'
        | '\u{e0000}'..='\u{e0fff}')
}

/// Spaces and tabs: what pads a command.
fn pads(c: char) -> bool {
    c == ' ' || c == '\t'
}

/// `text` with its padding counted instead of drawn, so no padding can push
/// the rest of a command off the screen: more than 2 blank lines in a row as
/// one `⟨n blank lines⟩` line, and more than 8 spaces or tabs in a row as
/// `⟨n spaces⟩`. Bash does not care how much space separates its words, and
/// inside quotes the count says exactly what is there.
fn squeezed(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut blank: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        if line.chars().all(pads) {
            blank.push(line);
            continue;
        }
        blank_lines(&mut out, &mut blank);
        out.push(counted_padding(line));
    }
    blank_lines(&mut out, &mut blank);
    out.join("\n")
}

/// A run of blank lines into `out`, as it is or counted.
fn blank_lines(out: &mut Vec<String>, run: &mut Vec<&str>) {
    if run.len() > 2 {
        out.push(format!("⟨{} blank lines⟩", run.len()));
    } else {
        out.extend(run.iter().map(|l| counted_padding(l)));
    }
    run.clear();
}

/// A line's indentation up to this many columns is drawn as it is: a
/// script's nesting reads as written, and no more than a row of it can stand
/// before a line.
const INDENT: usize = 40;

/// `line` with its indentation past [`INDENT`] columns, and each run of more
/// than 8 spaces or tabs inside it, counted.
fn counted_padding(line: &str) -> String {
    let body = line.trim_start_matches(pads);
    let indent = &line[..line.len() - body.len()];
    let tab = TAB.chars().count();
    let width: usize = indent
        .chars()
        .map(|c| if c == '\t' { tab } else { 1 })
        .sum();
    let mut out = if width <= INDENT {
        indent.to_owned()
    } else {
        counted(indent)
    };
    let mut run = String::new();
    for c in body.chars() {
        if pads(c) {
            run.push(c);
        } else {
            out.push_str(&counted(&run));
            run.clear();
            out.push(c);
        }
    }
    out + &counted(&run)
}

/// A run of spaces and tabs: as it is up to 8, counted past that.
fn counted(run: &str) -> String {
    let n = run.chars().count();
    if n <= 8 {
        run.to_owned()
    } else if !run.contains('\t') {
        format!("⟨{n} spaces⟩")
    } else if !run.contains(' ') {
        format!("⟨{n} tabs⟩")
    } else {
        format!("⟨{n} spaces and tabs⟩")
    }
}

#[cfg(test)]
mod tests;
