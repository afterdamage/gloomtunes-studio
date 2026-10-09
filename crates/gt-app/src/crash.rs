//! The opt-in crash reporter.
//!
//! When the user has turned it on, a panic on any thread writes a plain-text report into
//! `<data>/crashes/`. Nothing is sent anywhere: at the next start the app shows the report and
//! offers to open a pre-filled GitHub issue in the browser, which the user reads and submits
//! (or not) themselves.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

static ENABLED: AtomicBool = AtomicBool::new(false);
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// Longest report text put into an issue link; browsers and GitHub reject very long URLs.
const MAX_ISSUE_REPORT: usize = 5000;

/// Where reports go.
pub fn folder() -> PathBuf {
    crate::library::data_folder().join("crashes")
}

/// Installs the panic hook (once, at start). The default hook still runs first, so the
/// message also reaches stderr as before.
pub fn install(dir: PathBuf, enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
    let _ = DIR.set(dir);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if !ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let Some(dir) = DIR.get() else {
            return;
        };
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_owned());
        let location = info
            .location()
            .map_or_else(String::new, |l| format!("{}:{}", l.file(), l.line()));
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let text = report_text(&message, &location, &thread, &backtrace);
        // A failure to write the report must not cause a second panic.
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(dir.join(format!("crash-{}.txt", unix_time())), text);
    }));
}

/// Switches report writing on or off from now on.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The report: what happened, where, and on which build and system.
fn report_text(message: &str, location: &str, thread: &str, backtrace: &str) -> String {
    format!(
        "GloomTunes Studio crash report\n\
         version: {}\n\
         system: {} {}\n\
         time (unix): {}\n\
         thread: {thread}\n\
         message: {message}\n\
         location: {location}\n\
         \n\
         backtrace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        unix_time(),
    )
}

/// Saved reports, oldest first.
pub fn reports(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt"))
        })
        .collect();
    out.sort();
    out
}

/// A link that opens a new GitHub issue with the report filled in, for the user to review
/// and submit.
pub fn issue_url(report: &str) -> String {
    let first = report
        .lines()
        .find_map(|l| l.strip_prefix("message: "))
        .unwrap_or("crash");
    let title: String = format!("Crash: {first}").chars().take(100).collect();
    let mut cut: String = report.chars().take(MAX_ISSUE_REPORT).collect();
    if cut.len() < report.len() {
        cut.push_str("\n[… shortened; the full report is in the crashes folder]");
    }
    let body = format!(
        "**What were you doing when it happened?**\n\n\n\n**Crash report**\n\n```\n{cut}\n```\n"
    );
    format!(
        "{}/issues/new?labels=crash&title={}&body={}",
        env!("CARGO_PKG_REPOSITORY"),
        percent_encode(&title),
        percent_encode(&body)
    )
}

/// Percent-encodes everything but unreserved URL characters.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Opens a URL or folder with the desktop's default handler.
pub fn open_external(target: &str) -> std::io::Result<()> {
    use std::process::Command;
    let mut cmd = if cfg!(windows) {
        // Unlike `cmd /C start`, this passes `&` in URLs through untouched.
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", target]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(target);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };
    let mut child = cmd.spawn()?;
    // Reap the launcher when it exits, so no zombie stays behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_links_carry_the_encoded_report() {
        let report = report_text("index out of bounds & co", "src/a.rs:3", "main", "0: x");
        let url = issue_url(&report);
        assert!(url.starts_with("https://github.com/afterdamage/gloomtunes-studio/issues/new?"));
        assert!(url.contains("title=Crash%3A%20index%20out%20of%20bounds%20%26%20co"));
        assert!(url.contains("src%2Fa.rs%3A3"));
        assert!(!url[url.find('?').unwrap()..].contains(' '));
        // Long reports are cut so the link stays usable.
        let long = "x".repeat(50_000);
        assert!(issue_url(&long).len() < 3 * MAX_ISSUE_REPORT + 1000);
    }

    #[test]
    fn reports_are_listed_oldest_first() {
        let dir = std::env::temp_dir().join(format!("gt-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["crash-20.txt", "crash-10.txt", "other.txt"] {
            std::fs::write(dir.join(name), "r").unwrap();
        }
        let found = reports(&dir);
        let names: Vec<_> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["crash-10.txt", "crash-20.txt"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_panic_writes_a_report_only_when_enabled() {
        let dir = std::env::temp_dir().join(format!("gt-crash-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        install(dir.clone(), false);
        let _ = std::thread::Builder::new()
            .name("worker".into())
            .spawn(|| panic!("off"))
            .unwrap()
            .join();
        assert!(reports(&dir).is_empty());
        set_enabled(true);
        let _ = std::thread::Builder::new()
            .name("worker".into())
            .spawn(|| panic!("boom"))
            .unwrap()
            .join();
        set_enabled(false);
        let found = reports(&dir);
        assert_eq!(found.len(), 1);
        let text = std::fs::read_to_string(&found[0]).unwrap();
        assert!(text.contains("message: boom"), "{text}");
        assert!(text.contains("thread: worker"));
        assert!(text.contains("crash.rs"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
