use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use anyhow::{bail, Context, Result};

use crate::proc;
use crate::sdk::Sdk;

/// WSA publishes its ADB bridge on this loopback port. Unlike an AVD serial
/// (`emulator-5554`) it never shows up in `adb devices` until someone runs
/// `adb connect`, which is why the panel has to do it explicitly.
pub const SERIAL: &str = "127.0.0.1:58526";

/// Package identity Microsoft ships; MagiskOnWSALocal keeps the same name.
pub const PACKAGE: &str = "MicrosoftCorporationII.WindowsSubsystemForAndroid";

/// Folder the file browser opens first: the storage Android shares with apps.
pub const FILES_HOME: &str = "/sdcard";

/// Android build properties worth showing in the overview.
pub struct Info {
    pub release: String,
    pub abi: String,
    pub model: String,
    pub magisk: bool,
}

/// One row of `ls -lAp`, as shown by the WSA file browser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Dir,
    /// Symlink; Android's storage is mostly links, so these are entered too.
    Link,
    File,
}

impl Entry {
    pub fn enterable(&self) -> bool {
        !matches!(self.kind, Kind::File)
    }
}
/// A Windows Subsystem for Android install found on this machine.
#[derive(Clone, Debug)]
pub struct Wsa {
    /// Directory holding `AppxManifest.xml` and `WsaClient\`.
    pub root: PathBuf,
    /// Present for a loose MagiskOnWSALocal layout, absent for a store install.
    pub client: Option<PathBuf>,
    pub version: String,
}

impl Wsa {
    /// Locate WSA from the settings override, the running service, or the
    /// usual MagiskOnWSALocal locations. `None` means WSA is not installed.
    pub fn discover(override_root: &str) -> Option<Wsa> {
        let mut roots: Vec<PathBuf> = Vec::new();
        let override_root = override_root.trim();
        if !override_root.is_empty() {
            roots.push(PathBuf::from(override_root));
        }
        roots.extend(service_root());
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let home = PathBuf::from(home);
            roots.push(home.join("WSA"));
            roots.push(home.join("WSA-preview"));
        }
        for root in roots {
            if let Some(wsa) = Wsa::from_root(root) {
                return Some(wsa);
            }
        }
        None
    }

    /// Build a handle for an explicit install directory.
    pub fn from_root(root: PathBuf) -> Option<Wsa> {
        let client = [
            root.join("WsaClient").join("WsaClient.exe"),
            root.join("WsaClient.exe"),
        ]
        .into_iter()
        .find(|p| p.is_file());
        let manifest = root.join("AppxManifest.xml");
        // A folder is a usable source when it holds the loose layout, the
        // manifest to register, or a package file to install.
        if client.is_none() && !manifest.is_file() && package_file(&root).is_none() {
            return None;
        }
        let version = manifest_version(&manifest).unwrap_or_default();
        Some(Wsa {
            root,
            client,
            version,
        })
    }

    /// Loose-layout executable; a package-file install has none to show.
    pub fn client_exe(&self) -> Option<&Path> {
        self.client.as_deref()
    }

    /// Start the VM through the package's registered `wsa://` handler.
    ///
    /// Spawning `WsaClient.exe` directly does not work: the shell activation
    /// gives it the package identity it expects, while a bare CreateProcess
    /// dies with 0xC0000409 (ucrtbase BEX64, i.e. an unhandled abort) or, with
    /// the wrong working directory, cannot find `gfxstream_backend.dll`.
    pub fn start(&self) -> Result<()> {
        // Activate the registered wsa:// handler instead of CreateProcess on
        // WsaClient.exe: a bare spawn fails to find gfxstream_backend.dll and,
        // with the right cwd, still aborts with 0xC0000409 (ucrtbase) because
        // it expects package identity that only the shell activation provides.
        proc::detached("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", "wsa://systemsettings"])
            .current_dir(&self.root)
            .spawn()
            .context("activate wsa:// handler")?;
        Ok(())
    }

    /// Power the VM off.
    ///
    /// `reboot -p` stops Android but leaves `vmmemWSA` and `WsaClient` behind
    /// in most cases, so the RAM is never released. WsaClient's `/shutdown`
    /// does not work in that state either. After the guest goes down, stop the
    /// service and clear the leftovers so the next Start really is a cold boot.
    pub fn stop(&self, sdk: &Sdk) -> Result<()> {
        let adb = sdk.adb.as_ref().context("adb not found")?;
        proc::run(adb, &["-s", SERIAL, "shell", "reboot", "-p"])
            .context("power off WSA from inside Android")?;
        wait_port_closed(Duration::from_secs(20));
        proc::run_lossy(&PathBuf::from("sc.exe"), &["stop", "WsaService"]);
        for name in ["WsaClient.exe", "WsaSettings.exe"] {
            proc::run_lossy(&PathBuf::from("taskkill.exe"), &["/IM", name, "/F"]);
        }
        Ok(())
    }

    /// Ask the guest to reboot; the ADB transport drops, so reconnect after.
    pub fn restart(&self, sdk: &Sdk) -> Result<()> {
        let adb = sdk.adb.as_ref().context("adb not found")?;
        proc::run(adb, &["-s", SERIAL, "shell", "reboot"]).context("reboot WSA")?;
        Ok(())
    }
}

/// Wait until the ADB bridge stops accepting connections.
fn wait_port_closed(budget: Duration) {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !port_open() {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Register the ADB bridge and return adb's own message ("connected to …").
/// Free function so callers can run it without holding a `Wsa` handle.
pub fn connect(sdk: &Sdk) -> Result<String> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    proc::run(adb, &["connect", SERIAL])
}

/// Does something answer on the WSA ADB port?
///
/// `TcpClient::Connect` to a closed port burns the full OS SYN retry budget
/// (measured ~2 s here). This is called from the UI thread, so the probe is
/// bounded to 40 ms and reported as "not up" past that.
pub fn port_open() -> bool {
    let Ok(addr) = SERIAL.parse::<SocketAddr>() else {
        return false;
    };
    let Ok(client) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(40))
    else {
        return false;
    };
    drop(client);
    true
}

/// Poll until WSA answers ADB, or the budget runs out.
pub fn wait_ready(sdk: &Sdk, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if port_open() {
            let adb = match sdk.adb.as_ref() {
                Some(adb) => adb,
                None => return false,
            };
            proc::run_lossy(adb, &["connect", SERIAL]);
            let devices = crate::sdk::list_devices(sdk);
            if devices
                .iter()
                .any(|d| d.serial == SERIAL && d.state == "device")
            {
                return true;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    false
}

pub fn info(sdk: &Sdk) -> Option<Info> {
    let adb = sdk.adb.as_ref()?;
    let get = |prop: &str| {
        let key = format!("getprop {prop}");
        proc::run_lossy(adb, &["-s", SERIAL, "shell", &key])
            .trim()
            .to_string()
    };
    let release = get("ro.build.version.release");
    if release.is_empty() {
        return None;
    }
    Some(Info {
        release,
        abi: get("ro.product.cpu.abi"),
        model: get("ro.product.model"),
        magisk: !get("su -c id").is_empty(),
    })
}

/// Every package WSA reports as third-party (user-installed).
pub fn packages(sdk: &Sdk) -> Vec<String> {
    let Some(adb) = sdk.adb.as_ref() else {
        return Vec::new();
    };
    let text = proc::run_lossy(adb, &["-s", SERIAL, "shell", "pm list packages -3"]);
    let mut out: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("package:"))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Start an installed package via ADB `monkey`, which resolves the launcher
/// intent for us and reports cleanly when the package has none.
pub fn start_app(sdk: &Sdk, package: &str) -> Result<String> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    let cmd = format!("monkey -p {package} -c android.intent.category.LAUNCHER 1 2>&1 | tail -n 3");
    proc::run(adb, &["-s", SERIAL, "shell", &cmd])
        .map(|s| s.trim().to_string())
        .map_err(|e| anyhow::anyhow!(e.to_string().trim().to_string()))
}

/// Kill a running package. `am force-stop` also drops it from recents.
pub fn force_stop(sdk: &Sdk, package: &str) -> Result<String> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    let cmd = format!("am force-stop {package}");
    proc::run(adb, &["-s", SERIAL, "shell", &cmd])
}

/// Is the WSA package registered for this user? `None` means PowerShell itself
/// could not answer, so callers can fall back to a folder probe.
pub fn registered() -> Option<bool> {
    let script = format!(
        "@(Get-AppxPackage -Name '*{PACKAGE}*' -ErrorAction SilentlyContinue).Count"
    );
    let out = proc::cmd("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let count = String::from_utf8_lossy(&out.stdout);
    count.trim().parse::<u32>().ok().map(|n| n > 0)
}

/// Register the package with Windows.
///
/// MagiskOnWSALocal ships a loose file layout, so installing is an
/// `Add-AppxPackage -Register` run inside that folder; a folder holding an
/// `.msixbundle` is installed from the package file instead.
pub fn install(root: &Path) -> Result<String> {
    let script = if root.join("AppxManifest.xml").is_file() {
        "Add-AppxPackage -Register '.\\AppxManifest.xml' -ForceApplicationShutdown".to_string()
    } else if let Some(package) = package_file(root) {
        format!(
            "Add-AppxPackage -Path '{}' -ForceApplicationShutdown",
            package.display()
        )
    } else {
        bail!(
            "{} tidak berisi AppxManifest.xml atau .msixbundle — pilih folder hasil MagiskOnWSALocal",
            root.display()
        );
    };
    let out = proc::cmd("powershell.exe")
        .current_dir(root)
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .context("jalankan powershell untuk Add-AppxPackage")?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&err);
    }
    if !out.status.success() {
        bail!("Add-AppxPackage gagal: {}", text.trim());
    }
    Ok(text)
}

/// First `.msixbundle` / `.msix` / `.appx` in a folder, if any.
fn package_file(root: &Path) -> Option<PathBuf> {
    std::fs::read_dir(root)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    let ext = ext.to_ascii_lowercase();
                    ext == "msixbundle" || ext == "msix" || ext == "appx"
                })
                .unwrap_or(false)
        })
}

/// List one guest directory through ADB.
pub fn list_dir(sdk: &Sdk, path: &str) -> Result<Vec<Entry>> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    let command = format!("ls -lAp {}", shell_quote(path));
    match proc::run(adb, &["-s", SERIAL, "shell", &command]) {
        Ok(text) => Ok(parse_ls(&text)),
        Err(error) => bail!("{}", adb_message(&error)),
    }
}

/// Copy one file or folder from the guest into `dir` on this machine.
pub fn pull(sdk: &Sdk, remote: &str, dir: &Path) -> Result<PathBuf> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    std::fs::create_dir_all(dir).with_context(|| format!("buat {}", dir.display()))?;
    let name = remote
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("file");
    let local = dir.join(name);
    proc::run(
        adb,
        &["-s", SERIAL, "pull", remote, &local.to_string_lossy()],
    )
    .with_context(|| format!("pull {remote}"))?;
    Ok(local)
}

/// Join a remote directory and an entry name: `/sdcard` + `DCIM`.
pub fn child(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Parent of a remote path; the root stays the root.
pub fn parent(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => trimmed[..index].to_string(),
    }
}

/// Byte count for the browser's size column.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Parse `ls -lAp`. Names may contain spaces, so the name is whatever follows
/// the seventh column instead of an nth whitespace token.
fn parse_ls(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.is_empty() || line.starts_with("total ") || line.starts_with("total\t") {
            continue;
        }
        let columns: Vec<&str> = line.split_whitespace().collect();
        if columns.len() < 8 {
            continue;
        }
        let Some(rest) = after_columns(line, 7) else {
            continue;
        };
        let size = columns[4].parse::<u64>().unwrap_or(0);
        let name = rest
            .split(" -> ")
            .next()
            .unwrap_or(rest)
            .trim_end_matches('/');
        if name.is_empty() {
            continue;
        }
        let kind = match columns[0].chars().next() {
            Some('d') => Kind::Dir,
            Some('l') => Kind::Link,
            _ => Kind::File,
        };
        out.push(Entry {
            name: name.to_string(),
            kind,
            size,
        });
    }
    out.sort_by_key(|entry| (entry.kind == Kind::File, entry.name.to_lowercase()));
    out
}

/// Remainder of `line` after its first `count` whitespace-separated columns.
fn after_columns(line: &str, count: usize) -> Option<&str> {
    let mut seen = 0usize;
    let mut in_word = false;
    for (index, ch) in line.char_indices() {
        if ch.is_whitespace() {
            if in_word {
                seen += 1;
                in_word = false;
                if seen == count {
                    return Some(line[index..].trim_start());
                }
            }
        } else {
            in_word = true;
        }
    }
    None
}

/// Quote a path for the device shell: `adb shell` joins its argv without
/// quoting, so the path has to arrive already quoted.
fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

/// `proc::run` prefixes failures with the whole command line; the guest's own
/// message is what belongs in the panel.
fn adb_message(error: &anyhow::Error) -> String {
    let text = error.to_string();
    match text.split_once("): ") {
        Some((_, detail)) => detail.trim().to_string(),
        None => text,
    }
}

fn service_root() -> Option<PathBuf> {
    let out = proc::cmd("sc.exe").args(["qc", "WsaService"]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    root_from_sc_qc(&text)
}

/// Pull the install root out of `sc qc WsaService` output:
/// `...\WSA\WsaService\WsaService.exe` -> `...\WSA`.
fn root_from_sc_qc(text: &str) -> Option<PathBuf> {
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() != "BINARY_PATH_NAME" {
            continue;
        }
        let value = value.trim().trim_matches('"');
        let exe = PathBuf::from(value);
        return exe.parent().and_then(Path::parent).map(Path::to_path_buf);
    }
    None
}

fn manifest_version(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    version_from_manifest(&text)
}

/// Read `Version="…"` from AppxManifest.xml. The identity block is the first
/// thing in the file, so the first match is the package version.
fn version_from_manifest(text: &str) -> Option<String> {
    let start = text.find("Version=\"")? + "Version=\"".len();
    let rest = &text[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Fail loudly when the install cached by `discover` has gone away.
pub fn ensure_client(wsa: &Wsa) -> Result<()> {
    if wsa.root.is_dir() {
        return Ok(());
    }
    bail!("folder WSA tidak ada lagi: {}", wsa.root.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SC_QC: &str = "\r\nSERVICE_NAME: WsaService\r\n\
        \x20       TYPE               : 210  WIN32_PACKAGED_PROCESS\r\n\
        \x20       BINARY_PATH_NAME   : \"C:\\Users\\dev\\WSA\\WsaService\\WsaService.exe\"\r\n\
        \x20       DISPLAY_NAME       : Windows Subsystem for Android\r\n";

    #[test]
    fn service_root_strips_two_levels_and_quotes() {
        let root = root_from_sc_qc(SC_QC).unwrap();
        assert_eq!(root, PathBuf::from(r"C:\Users\dev\WSA"));
    }

    #[test]
    fn service_root_is_none_without_binary_path() {
        assert!(root_from_sc_qc("SERVICE_NAME: WsaService\r\n").is_none());
    }

    #[test]
    fn service_root_ignores_unrelated_path_key() {
        // DISPLAY_NAME must not be mistaken for the binary path.
        let text = "        DISPLAY_NAME       : C:\\x\\WsaService.exe\r\n";
        assert!(root_from_sc_qc(text).is_none());
    }

    #[test]
    fn manifest_version_reads_identity() {
        let xml = r#"<Package><Identity Name="A" Version="2407.40000.4.0" /><Other Version="1.0"/></Package>"#;
        assert_eq!(version_from_manifest(xml).as_deref(), Some("2407.40000.4.0"));
    }

    #[test]
    fn manifest_version_is_none_when_absent() {
        assert!(version_from_manifest("<Package />").is_none());
    }

    #[test]
    fn serial_matches_the_adb_bridge_port() {
        assert_eq!(SERIAL, "127.0.0.1:58526");
    }
    const LS: &str = "total 24\r\n\
        drwxrwx--x   2 root     sdcard_rw    4096 2024-05-01 10:00 Alarms\r\n\
        -rw-rw----   1 root     sdcard_rw   12345 2024-05-01 10:00 notes.txt\r\n\
        lrwxrwxrwx   1 root     root           21 2024-05-01 10:00 sdcard -> /storage/self/primary\r\n";

    #[test]
    fn ls_parses_dirs_files_and_links() {
        let entries = parse_ls(LS);
        assert_eq!(entries.len(), 3);
        let dir = entries.iter().find(|e| e.name == "Alarms").unwrap();
        assert_eq!(dir.kind, Kind::Dir);
        assert!(dir.enterable());
        let file = entries.iter().find(|e| e.name == "notes.txt").unwrap();
        assert_eq!(file.kind, Kind::File);
        assert_eq!(file.size, 12345);
        assert!(!file.enterable());
        let link = entries.iter().find(|e| e.name == "sdcard").unwrap();
        assert_eq!(link.kind, Kind::Link);
        assert!(link.enterable());
        // Directories and links sort ahead of plain files.
        assert_eq!(entries.last().unwrap().name, "notes.txt");
    }

    #[test]
    fn ls_keeps_spaces_in_names() {
        let text = "-rw-rw---- 1 root sdcard_rw 7 2024-05-01 10:00 two words.txt\r\n";
        let entries = parse_ls(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "two words.txt");
    }

    #[test]
    fn ls_skips_total_and_junk() {
        assert!(parse_ls("total 4\r\n\r\n").is_empty());
        assert!(parse_ls("garbage line\r\n").is_empty());
    }

    #[test]
    fn child_and_parent_walk_the_remote_tree() {
        assert_eq!(child("/sdcard", "DCIM"), "/sdcard/DCIM");
        assert_eq!(child("/", "sdcard"), "/sdcard");
        assert_eq!(parent("/sdcard/DCIM"), "/sdcard");
        assert_eq!(parent("/sdcard"), "/");
        assert_eq!(parent("/"), "/");
    }

    #[test]
    fn sizes_are_humanised() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn adb_errors_drop_the_command_prefix() {
        let error = anyhow::anyhow!(
            "C:\\sdk\\adb.exe -s 127.0.0.1:58526 shell ls failed (exit status: 1): \
             ls: /nope: No such file or directory"
        );
        assert_eq!(adb_message(&error), "ls: /nope: No such file or directory");
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("/sdcard/My Files"), "'/sdcard/My Files'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn manifest_or_bundle_is_enough_for_a_source_folder() {
        // No files on disk can satisfy this, so the probe must refuse.
        assert!(Wsa::from_root(PathBuf::from(r"C:\definitely\not\here")).is_none());
    }
}
