//! Tool discovery. GUI apps on macOS start with a minimal PATH (`/usr/bin:/bin:...`), so
//! `claude` installed via npm, Homebrew or the native installer is usually not on it.
//! We ask the user's login shell for its PATH once and use that for every child process.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{AppError, AppResult};

static LOGIN_PATH: OnceLock<String> = OnceLock::new();
const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// PATH from the user's login shell, merged with the current process PATH and common install dirs.
pub fn login_path() -> &'static str {
    LOGIN_PATH.get_or_init(|| {
        let mut parts: Vec<String> = Vec::new();
        if let Some(p) = shell_path() {
            parts.extend(p.split(':').map(str::to_string));
        }
        if let Ok(p) = std::env::var("PATH") {
            parts.extend(p.split(':').map(str::to_string));
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for rel in [
                ".local/bin",
                ".claude/local",
                ".npm-global/bin",
                ".volta/bin",
                ".bun/bin",
            ] {
                parts.push(home.join(rel).to_string_lossy().into_owned());
            }
        }
        parts.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(String::from));
        let mut seen = std::collections::HashSet::new();
        parts.retain(|p| !p.is_empty() && seen.insert(p.clone()));
        parts.join(":")
    })
}

fn shell_path() -> Option<String> {
    const MARKER: &str = "__PRH__";
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    // `-i` loads rc files where tools like nvm add themselves; the marker isolates PATH
    // from anything those files print.
    let mut child = std::process::Command::new(shell)
        .arg("-ilc")
        .arg(format!("printf '{MARKER}%s{MARKER}' \"$PATH\""))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout on a thread so chatty rc files can't fill the pipe and stall the shell.
    let mut pipe = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
        let _ = tx.send(buf);
    });
    // An rc file that blocks (`exec tmux`, a slow network call) must not hang the app:
    // give up after a few seconds and fall back to the static PATH.
    let deadline = std::time::Instant::now() + SHELL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            _ => {
                tracing::warn!("login shell did not report its PATH in time; using defaults");
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    // A daemon started by an rc file can keep the pipe open after the shell exits.
    let stdout = rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .inspect_err(|_| tracing::warn!("login shell kept its output open; using defaults"))
        .ok()?;
    let text = String::from_utf8_lossy(&stdout);
    let (_, rest) = text.split_once(MARKER)?;
    let (path, _) = rest.split_once(MARKER)?;
    Some(path.to_string())
}

fn which_in(name: &str, path: &str) -> Option<PathBuf> {
    path.split(':')
        .map(|dir| Path::new(dir).join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Resolve a tool from an explicit override or the login PATH.
pub fn resolve_tool(name: &str, override_path: Option<&str>) -> AppResult<PathBuf> {
    if let Some(p) = override_path.map(str::trim).filter(|p| !p.is_empty()) {
        let p = PathBuf::from(p);
        return if is_executable(&p) {
            Ok(p)
        } else {
            Err(AppError::invalid(format!(
                "{} is not an executable file",
                p.display()
            )))
        };
    }
    which_in(name, login_path()).ok_or_else(|| {
        AppError::invalid(format!(
            "could not find `{name}` on your PATH; set its location in Settings"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_executables_on_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fake-tool");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("/nonexistent:{}", dir.path().display());
        assert_eq!(which_in("fake-tool", &path), Some(bin.clone()));
        assert_eq!(which_in("missing-tool", &path), None);
        assert_eq!(resolve_tool("x", Some(bin.to_str().unwrap())).unwrap(), bin);
        assert!(resolve_tool("x", Some("/nonexistent/bin")).is_err());
    }
}
