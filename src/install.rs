// Installs Nova Musique from its repo. One implementation, shared by build.rs (which installs at
// build time) and the app binary (which reinstalls when run).
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

pub const REPO: &str = "https://github.com/SarK0Y/Nova_Musique.git";
pub const BRANCH: &str = "main";
const BINS: &[&str] = &["nova", "nova-cli"];

/// Clones branch `main` into `./nova`, falling back to `/tmp` when that fails (directory
/// already there, not writable, no current directory, ...) and returns the download directory.
pub fn download() -> Result<PathBuf, String> {
    if let Ok(cwd) = env::current_dir() {
        let primary = cwd.join("nova");
        if clone_into(&primary).is_ok() {
            return Ok(primary);
        }
        say(&format!(
            "cannot clone into {}, falling back to /tmp",
            primary.display()
        ));
    }

    let fallback = PathBuf::from("/tmp").join(format!("install-nova-musique-{}", std::process::id()));
    if fallback.exists() {
        fs::remove_dir_all(&fallback).map_err(|e| format!("cannot clean {}: {e}", fallback.display()))?;
    }
    clone_into(&fallback)?;
    Ok(fallback)
}

fn clone_into(dir: &Path) -> Result<(), String> {
    say(&format!("downloading {REPO} (branch {BRANCH}) into {}", dir.display()));
    let mut git = Command::new("git");
    git.args(["clone", "--progress", "--depth", "1", "--branch", BRANCH, "--single-branch", REPO])
        .arg(dir);
    run(&mut git, "git clone")?;

    let mut list = Command::new("git");
    list.arg("-C").arg(dir).arg("ls-files");
    let files = capture(&mut list, "git ls-files")?;
    let files: Vec<&str> = files.lines().collect();
    for file in &files {
        say(&format!("downloaded {file}"));
    }
    say(&format!("downloaded {} files into {}", files.len(), dir.display()));
    Ok(())
}

/// Builds the download and copies its binaries into the install directory.
pub fn build_and_install(src: &Path) -> Result<(), String> {
    // shortcut: the download is erased after each install, so every run rebuilds from scratch;
    // keep a build cache outside it to speed up reinstalls
    say("compiling Nova Musique (release), this can take several minutes...");
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut build = Command::new(cargo);
    build
        .current_dir(src)
        // a nested build must never reuse the outer build's target dir: it deadlocks on its lock
        .env_remove("CARGO_TARGET_DIR")
        .args(["build", "--release"]);
    if let Err(e) = run(&mut build, "cargo build --release") {
        return Err(format!("{e}; downloaded sources kept at {}", src.display()));
    }

    let dest = bin_dir()?;
    fs::create_dir_all(&dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let built = src.join("target").join("release");
    for name in bin_names() {
        let from = built.join(&name);
        if !from.is_file() {
            return Err(format!(
                "{} was not built; downloaded sources kept at {}",
                from.display(),
                src.display()
            ));
        }
        let to = dest.join(&name);
        fs::copy(&from, &to).map_err(|e| format!("cannot copy to {}: {e}", to.display()))?;
        say(&format!("installed {}", to.display()));
    }
    Ok(())
}

/// Erases the downloaded files — only ever called after a successful install.
pub fn erase(src: &Path) -> Result<(), String> {
    fs::remove_dir_all(src)
        .map_err(|e| format!("installed, but could not erase {}: {e}", src.display()))?;
    say(&format!("erased downloaded files in {}", src.display()));
    Ok(())
}

/// The whole flow: download, install, and only then erase.
pub fn install() -> Result<(), String> {
    let src = download()?;
    build_and_install(&src)?;
    erase(&src)
}

/// Everything the user must see. The app prints to stdout; a build script writes to the
/// terminal directly, because cargo buffers a build script's output until it exits — useless
/// for an install that runs for minutes. shortcut: tty lines don't land in cargo's captured
/// output, the cargo:warning fallback covers runs without a terminal.
fn say(line: &str) {
    if env::var_os("OUT_DIR").is_some() {
        if !cfg!(windows) {
            if let Ok(mut tty) = fs::OpenOptions::new().write(true).open("/dev/tty") {
                let _ = writeln!(tty, "{line}");
                return;
            }
        }
        let mut out = std::io::stdout();
        let _ = writeln!(out, "cargo:warning={line}");
        return;
    }
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
}

/// Runs a command with its output shown live. The app lets the child write to the terminal
/// itself; inside a build script cargo swallows that output, so each line is relayed via say().
fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    if env::var_os("OUT_DIR").is_none() {
        let status = cmd.status().map_err(|e| format!("{what} could not run: {e}"))?;
        return finished(status, what);
    }
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{what} could not run: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let readers = [
        thread::spawn(move || relay(stdout)),
        thread::spawn(move || relay(stderr)),
    ];
    for reader in readers {
        let _ = reader.join();
    }
    let status = child.wait().map_err(|e| format!("{what} could not run: {e}"))?;
    finished(status, what)
}

fn capture(cmd: &mut Command, what: &str) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("{what} could not run: {e}"))?;
    if !out.status.success() {
        return Err(format!("{what} failed with {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn finished(status: ExitStatus, what: &str) -> Result<(), String> {
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed with {status}"))
    }
}

/// Streams child output to the user as it arrives; git moves progress along \r, cargo uses \n.
fn relay(mut src: impl Read) {
    let mut carry: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => carry.extend_from_slice(&buf[..n]),
        }
        while let Some(line) = next_line(&mut carry) {
            if !line.is_empty() {
                say(&String::from_utf8_lossy(&line));
            }
        }
    }
    if !carry.is_empty() {
        say(&String::from_utf8_lossy(&carry));
    }
}

/// One line without its \r or \n terminator; an incomplete tail stays in `carry`.
fn next_line(carry: &mut Vec<u8>) -> Option<Vec<u8>> {
    let end = carry.iter().position(|&b| b == b'\n' || b == b'\r')?;
    let mut line: Vec<u8> = carry.drain(..=end).collect();
    line.pop(); // the terminator itself
    Some(line)
}

fn bin_dir() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or_else(|| "HOME is not set, cannot find an install directory".to_string())?;
    Ok(PathBuf::from(home).join(".local/bin"))
}

fn bin_names() -> Vec<String> {
    BINS.iter()
        .map(|b| if cfg!(windows) { format!("{b}.exe") } else { b.to_string() })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_names_match_the_workspace_bins() {
        let names = bin_names();
        assert_eq!(names.len(), BINS.len());
        for name in &names {
            assert!(name.starts_with("nova"), "{name}");
            assert_eq!(name.ends_with(".exe"), cfg!(windows), "{name}");
        }
        // install dir must be absolute or the binaries land somewhere relative to nothing
        assert!(bin_dir().expect("HOME").is_absolute());
    }

    #[test]
    fn progress_lines_split_on_carriage_return_and_newline() {
        let mut carry = b"Receiving objects: 50%\r\n   Compiling eframe v0.35.0\npar".to_vec();
        let mut lines = Vec::new();
        while let Some(line) = next_line(&mut carry) {
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        assert_eq!(lines, ["Receiving objects: 50%", "", "   Compiling eframe v0.35.0"]);
        // git progress splits mid-line, the incomplete tail waits for the next read
        assert_eq!(carry, b"par");
    }
}
