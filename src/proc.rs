use std::ffi::OsStr;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use anyhow::{bail, Context, Result};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Command builder for short-lived tools (adb, avdmanager): no console flash.
pub fn cmd(program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Command builder for long-lived GUI processes such as the emulator.
///
/// `emulator.exe` is a CONSOLE-subsystem binary, so spawning it from a
/// GUI-subsystem app hands it a fresh visible console window. `CREATE_NO_WINDOW`
/// suppresses that, and a new process group keeps it out of ours.
pub fn detached(program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    c
}

fn text(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&err);
    }
    s
}

/// Run a tool, fail on non-zero exit, return combined output.
pub fn run(program: &Path, args: &[&str]) -> Result<String> {
    let out = cmd(program)
        .args(args)
        .output()
        .with_context(|| format!("spawn {} {:?}", program.display(), args))?;
    let s = text(&out);
    if !out.status.success() {
        bail!(
            "{} {} failed ({}): {}",
            program.display(),
            args.join(" "),
            out.status,
            s.trim()
        );
    }
    Ok(s)
}

/// Run a tool and return combined output regardless of exit status.
pub fn run_lossy(program: &Path, args: &[&str]) -> String {
    match cmd(program).args(args).output() {
        Ok(out) => text(&out),
        Err(e) => format!("spawn {} failed: {e}", program.display()),
    }
}

/// Run a tool that writes binary to stdout.
pub fn run_bytes(program: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = cmd(program)
        .args(args)
        .output()
        .with_context(|| format!("spawn {} {:?}", program.display(), args))?;
    if !out.status.success() {
        bail!(
            "{} {} failed ({}): {}",
            program.display(),
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// Run a tool feeding stdin (used by avdmanager, which asks about hardware profiles).
pub fn run_stdin(program: &Path, args: &[&str], stdin_text: &str) -> Result<String> {
    let mut child = cmd(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {} {:?}", program.display(), args))?;
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(stdin_text.as_bytes());
    }
    let out = child.wait_with_output().context("wait for tool")?;
    let s = text(&out);
    if !out.status.success() {
        bail!(
            "{} {} failed ({}): {}",
            program.display(),
            args.join(" "),
            out.status,
            s.trim()
        );
    }
    Ok(s)
}

/// Open a path with the platform file browser.
pub fn reveal(path: &Path) -> Result<()> {
    #[cfg(windows)]
    let mut c = detached("explorer.exe");
    #[cfg(target_os = "macos")]
    let mut c = detached("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut c = detached("xdg-open");
    c.arg(path).spawn().context("open file browser")?;
    Ok(())
}
