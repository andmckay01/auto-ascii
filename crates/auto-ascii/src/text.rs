//! Text from remote pages and shipped clip folders, made safe to print on a
//! terminal.

use std::borrow::Cow;

const KEPT_CONTROLS: [char; 2] = ['\n', '\t'];

const KEPT_IN_ONE_LINE: [char; 1] = ['\t'];

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

#[cfg(test)]
mod tests {
    use super::*;

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
