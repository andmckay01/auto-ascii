use std::borrow::Cow;
use std::io::Write;

const KEPT_CONTROLS: [char; 2] = ['\n', '\t'];

const KEPT_IN_ONE_LINE: [char; 1] = ['\t'];

pub fn emit(text: &str) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        emit_err(&format!("auto-ascii: write to stdout failed: {e}\n"));
        std::process::exit(1);
    }
}

pub fn emit_err(text: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(text.as_bytes()).and_then(|()| err.flush());
}

macro_rules! outln {
    ($($arg:tt)*) => {{
        let mut line = std::fmt::format(format_args!($($arg)*));
        line.push('\n');
        $crate::output::emit(&line);
    }};
}

macro_rules! out {
    ($($arg:tt)*) => { $crate::output::emit(&std::fmt::format(format_args!($($arg)*))) };
}

pub fn terminal_safe(text: &str) -> Cow<'_, str> {
    scrub(text, &KEPT_CONTROLS)
}

pub fn terminal_safe_line(text: &str) -> Cow<'_, str> {
    scrub(text, &KEPT_IN_ONE_LINE)
}

fn scrub<'a>(text: &'a str, kept: &[char]) -> Cow<'a, str> {
    let inert = |c: char| !c.is_control() || kept.contains(&c);
    if text.chars().all(inert) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.chars().map(|c| if inert(c) { c } else { '?' }).collect())
}

pub fn fail(message: &str, json: bool) {
    let message = terminal_safe(message);
    if json {
        let obj = serde_json::json!({ "error": message });
        emit_err(&format!("{obj}\n"));
    } else {
        emit_err(&format!("auto-ascii: {message}\n"));
    }
}

pub fn fps_text(fps: f64) -> String {
    if (fps - fps.round()).abs() < 1e-9 {
        format!("{}", fps.round() as u64)
    } else {
        format!("{fps:.3}")
    }
}

pub fn human_bytes(n: u64) -> String {
    const KIB: f64 = 1024.0;
    let v = n as f64;
    if v >= KIB * KIB {
        format!("{:.1} MiB", v / (KIB * KIB))
    } else if v >= KIB {
        format!("{:.1} KiB", v / KIB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_formatting() {
        assert_eq!(fps_text(30.0), "30");
        assert_eq!(fps_text(29.97), "29.970");
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
    }

    #[test]
    fn control_characters_print_as_question_marks() {
        assert_eq!(terminal_safe("404: \x1b]0;X\x07 gone"), "404: ?]0;X? gone");
        assert_eq!(terminal_safe("50%\r100%"), "50%?100%");
        assert_eq!(terminal_safe("\u{9b}31mred\x7f"), "?31mred?");
        assert_eq!(terminal_safe("\x1b[2J\u{85}café ✓\x00"), "?[2J?café ✓?");
    }

    #[test]
    fn a_single_line_also_loses_its_newlines_but_keeps_tabs() {
        assert_eq!(
            terminal_safe_line("https://evil.example/v\nauto-ascii: forged"),
            "https://evil.example/v?auto-ascii: forged"
        );
        assert_eq!(terminal_safe_line("a\tb\r\nc\x1b]0;X\x07"), "a\tb??c?]0;X?");
    }

    #[test]
    fn clean_text_and_unicode_line_separators_pass_through_borrowed() {
        for clean in ["line one\nline two\tcafé ✓", "yt-dlp: HTTP Error 404: Not Found", "a\u{2028}b\u{2029}c", ""] {
            assert!(matches!(terminal_safe(clean), Cow::Borrowed(s) if s == clean), "{clean:?}");
        }
        for clean in ["one line\tcafé ✓", "a\u{2028}b\u{2029}c", ""] {
            assert!(matches!(terminal_safe_line(clean), Cow::Borrowed(s) if s == clean), "{clean:?}");
        }
    }

    #[test]
    fn only_c0_del_and_c1_change_and_a_single_line_keeps_no_newline() {
        for c in (0..=0x2FF).chain([0x2028, 0x2029]).filter_map(char::from_u32) {
            let text = c.to_string();
            let control = c < ' ' || ('\u{7f}'..='\u{9f}').contains(&c);
            let expected = |kept: bool| if kept { text.clone() } else { "?".to_string() };
            assert_eq!(terminal_safe(&text), expected(!control || c == '\n' || c == '\t'), "{c:?}");
            assert_eq!(terminal_safe_line(&text), expected(!control || c == '\t'), "{c:?}");
        }
    }
}
