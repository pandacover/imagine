//! Finding the ffmpeg program.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The ffmpeg program to run, found once per process: `FILMCRAFT_FFMPEG`, then next to the
/// executable (and the macOS bundle's `Resources`), then `PATH`, then the Homebrew and system
/// folders (apps started from the macOS Finder do not get the shell's `PATH`). A candidate counts
/// only when `ffmpeg -version` runs and succeeds.
pub fn binary() -> Option<&'static Path> {
    static BIN: OnceLock<Option<PathBuf>> = OnceLock::new();
    BIN.get_or_init(find).as_deref()
}

fn exe_name() -> &'static str {
    if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("FILMCRAFT_FFMPEG").filter(|p| !p.is_empty()) {
        out.push(PathBuf::from(p));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        out.push(dir.join(exe_name()));
        out.push(dir.join("..").join("Resources").join(exe_name()));
    }
    if let Some(path) = std::env::var_os("PATH") {
        out.extend(std::env::split_paths(&path).map(|d| d.join(exe_name())));
    }
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        out.push(Path::new(d).join(exe_name()));
    }
    out
}

fn find() -> Option<PathBuf> {
    candidates().into_iter().find(|p| p.is_file() && runs(p))
}

/// Whether `p` is a working ffmpeg.
fn runs(p: &Path) -> bool {
    let mut c = Command::new(p);
    c.args(["-hide_banner", "-version"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    crate::decoder::no_window(&mut c);
    c.status().is_ok_and(|s| s.success())
}
