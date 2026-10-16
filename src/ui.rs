//! Minimal terminal output helpers.

use std::io::IsTerminal;

pub fn is_tty() -> bool {
    std::io::stderr().is_terminal()
}

fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn green(s: &str) -> String {
    paint("32", s)
}

pub fn yellow(s: &str) -> String {
    paint("33", s)
}

pub fn red(s: &str) -> String {
    paint("31", s)
}

pub fn dim(s: &str) -> String {
    paint("2", s)
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}

pub fn ok(msg: impl AsRef<str>) {
    println!("{} {}", green("✓"), msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    println!("{} {}", yellow("!"), msg.as_ref());
}

pub fn fail(msg: impl AsRef<str>) {
    println!("{} {}", red("✗"), msg.as_ref());
}

/// Prints rows as left-aligned columns. Widths ignore ANSI escapes.
pub fn table(header: &[&str], rows: &[Vec<String>]) {
    let visible = |s: &str| {
        let mut n = 0;
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (false, _) => n += 1,
                _ => {}
            }
        }
        n
    };
    let cols = header.len();
    let mut widths: Vec<usize> = header.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(visible(cell));
        }
    }
    let line = |cells: Vec<String>| {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            out.push_str(cell);
            if i + 1 < cells.len() {
                out.push_str(&" ".repeat(widths[i] - visible(cell) + 2));
            }
        }
        println!("{}", out.trim_end());
    };
    line(header.iter().map(|h| bold(h)).collect());
    for row in rows {
        line(row.clone());
    }
}
