//! Tiny shared TTY status output — the `==>` / `✓` / `!` prefixes the
//! interactive commands print. Matches `install-cli.sh`'s style.

pub fn say(msg: &str) {
    println!("\x1b[1;36m==>\x1b[0m {msg}");
}
pub fn ok(msg: &str) {
    println!("\x1b[1;32m  ✓ \x1b[0m {msg}");
}
pub fn warn(msg: &str) {
    println!("\x1b[1;33m  ! \x1b[0m {msg}");
}
pub fn print_hint(hint: &str) {
    for line in hint.lines() {
        println!("      {line}");
    }
}
