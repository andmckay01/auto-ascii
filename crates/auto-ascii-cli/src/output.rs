use std::io::Write;

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

pub fn fail(message: &str, json: bool) {
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
}
