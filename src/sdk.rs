use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::proc;

/// Location of the pieces of the Android SDK this tool drives.
#[derive(Clone, Debug)]
pub struct Sdk {
    pub root: PathBuf,
    pub emulator: Option<PathBuf>,
    pub adb: Option<PathBuf>,
    pub avdmanager: Option<PathBuf>,
    pub sdkmanager: Option<PathBuf>,
    pub avd_home: PathBuf,
    pub java: Option<PathBuf>,
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<&str> = if cfg!(windows) {
        vec!["exe", "bat", "cmd"]
    } else {
        vec![""]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let cand = if ext.is_empty() {
                dir.join(name)
            } else {
                dir.join(format!("{name}.{ext}"))
            };
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn sdk_root_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["ANDROID_SDK_ROOT", "ANDROID_HOME", "ANDROID_SDK"] {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        out.push(PathBuf::from(local).join("Android").join("Sdk"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        out.push(home.join("Android").join("Sdk"));
        out.push(home.join("Library").join("Android").join("sdk"));
    }
    out
}

/// JDKs this app accepts, in preference order. `JAVA_HOME` is checked last
/// because on some machines it points at JDK 8 while the 17 lives in
/// `JAVA_HOME_17`.
const JDK_ENV_VARS: [&str; 4] = ["JAVA_HOME_17", "JDK17", "JAVA17_HOME", "JAVA_HOME"];

/// Every JDK directory the environment advertises, best candidate first.
fn java_home_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for var in JDK_ENV_VARS {
        if let Some(v) = std::env::var_os(var) {
            let p = PathBuf::from(v);
            if !p.as_os_str().is_empty() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// `java.exe` inside a JDK home.
fn java_exe(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("bin").join("java.exe")
    } else {
        home.join("bin").join("java")
    }
}

/// Best JDK home whose `bin/java` actually exists.
fn pick_java_home() -> Option<PathBuf> {
    java_home_candidates()
        .into_iter()
        .find(|h| java_exe(h).is_file())
}

/// Major version of the JDK at `home`, from its `release` file.
fn java_major(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("JAVA_VERSION=") else { continue };
        let v = rest.trim().trim_matches('"');
        let major = match v.strip_prefix("1.") {
            Some(rest) => rest,
            None => v,
        };
        if let Ok(n) = major.split('.').next()?.parse::<u32>() {
            return Some(n);
        }
    }
    None
}

fn avd_home_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(v) = std::env::var_os("ANDROID_AVD_HOME") {
        out.push(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("ANDROID_USER_HOME") {
        out.push(PathBuf::from(v).join("avd"));
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        out.push(PathBuf::from(home).join(".android").join("avd"));
    }
    out
}

impl Sdk {
    /// Discover the SDK from the environment, falling back to the usual install paths.
    pub fn discover() -> Sdk {
        let root = sdk_root_candidates()
            .into_iter()
            .find(|p| p.is_dir())
            .unwrap_or_default();
        Sdk::from_root(root)
    }

    /// Build an SDK handle for an explicit root directory.
    pub fn from_root(root: PathBuf) -> Sdk {
        let avd_home = avd_home_candidates()
            .into_iter()
            .find(|p| p.is_dir())
            .or_else(|| avd_home_candidates().into_iter().next())
            .unwrap_or_default();

        let emulator = [
            root.join("emulator").join("emulator.exe"),
            root.join("emulator").join("emulator"),
        ]
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| which("emulator"));

        let adb = [
            root.join("platform-tools").join("adb.exe"),
            root.join("platform-tools").join("adb"),
        ]
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| which("adb"));

        let avdmanager = find_cmdline_tool(&root, "avdmanager");
        let sdkmanager = find_cmdline_tool(&root, "sdkmanager");

        let java = pick_java_home().map(|home| java_exe(&home));

        Sdk {
            root,
            emulator,
            adb,
            avdmanager,
            sdkmanager,
            avd_home,
            java,
        }
    }

    pub fn missing(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.emulator.is_none() {
            v.push("emulator");
        }
        if self.adb.is_none() {
            v.push("adb");
        }
        if self.avdmanager.is_none() {
            v.push("avdmanager");
        }
        v
    }
}

/// What the app cannot run without, spelled out for a human.
pub struct Preflight {
    pub missing: Vec<String>,
    pub sdk_root: PathBuf,
    pub has_sdk: bool,
    pub has_java: bool,
    /// Set when a JDK exists but is too old (e.g. JAVA_HOME points at JDK 8).
    pub java_too_old: Option<(PathBuf, u32)>,
}

impl Preflight {
    pub fn message(&self) -> String {
        let mut out = String::new();
        out.push_str("devmgr tidak bisa dijalankan karena komponen berikut belum ada:\n\n");
        for item in &self.missing {
            out.push_str("  - ");
            out.push_str(item);
            out.push('\n');
        }
        out.push_str("\n");

        if !self.has_sdk {
            if self.sdk_root.as_os_str().is_empty() {
                out.push_str("Android SDK tidak ditemukan di lokasi mana pun.\n\n");
            } else {
                out.push_str(&format!(
                    "Android SDK tidak ditemukan.\nDicari di: {}\n\n",
                    self.sdk_root.display()
                ));
            }
            out.push_str("Cara memperbaiki:\n");
            out.push_str("  1. Install Android Studio, lalu buka SDK Manager dan pastikan\n");
            out.push_str("     Android SDK Platform-Tools, Android Emulator, dan satu\n");
            out.push_str("     system image sudah ter-download.\n");
            out.push_str("  2. Atau set environment variable ANDROID_SDK_ROOT ke folder SDK.\n");
            out.push('\n');
        }

        if !self.has_java {
            match &self.java_too_old {
                Some((home, major)) => {
                    out.push_str(&format!(
                        "Java yang ditemukan terlalu lama: JDK {major}\n  di {}\n\n",
                        home.display()
                    ));
                    out.push_str("devmgr butuh JDK 17 atau lebih baru (avdmanager adalah program Java).\n");
                    out.push_str("\nCara memperbaiki:\n");
                    out.push_str("  1. Install JDK 17 (misal Eclipse Temurin / Adoptium).\n");
                    out.push_str("  2. Set JAVA_HOME ke folder JDK 17 tersebut, misal:\n");
                    out.push_str("     C:\\Program Files\\Eclipse Adoptium\\jdk-17.0.x-hotspot\n");
                    out.push_str("  3. Kalau JAVA_HOME dipakai tool lain, set JAVA_HOME_17 saja.\n");
                    out.push_str("     devmgr membaca JAVA_HOME_17 lebih dulu daripada JAVA_HOME.\n");
                    out.push('\n');
                }
                None => {
                    out.push_str("Java JDK 17 tidak ditemukan.\n");
                    let seen: Vec<String> = java_home_candidates()
                        .into_iter()
                        .map(|p| p.display().to_string())
                        .collect();
                    if seen.is_empty() {
                        out.push_str("Tidak ada JAVA_HOME_17 / JAVA_HOME yang diset.\n");
                    } else {
                        out.push_str(&format!(
                            "Dicek (tidak ada bin\\java.exe): {}\n",
                            seen.join(", ")
                        ));
                    }
                    out.push_str("\nCara memperbaiki:\n");
                    out.push_str("  1. Install JDK 17 (misal Eclipse Temurin / Adoptium).\n");
                    out.push_str("  2. Set JAVA_HOME_17 atau JAVA_HOME ke folder JDK tersebut, misal:\n");
                    out.push_str("     C:\\Program Files\\Eclipse Adoptium\\jdk-17.0.x-hotspot\n");
                    out.push_str("  3. Pastikan %JAVA_HOME_17%\\bin\\java.exe ada.\n");
                    out.push('\n');
                }
            }
        }
        out.push_str("Setelah itu jalankan ulang devmgr.exe.");
        out
    }
}

/// Check the environment before any window is created.
pub fn preflight() -> Option<Preflight> {
    let sdk = Sdk::discover();
    let has_sdk = sdk.emulator.is_some() && sdk.adb.is_some();

    // A JDK that exists but is too old is a different problem from a missing one.
    let java_home = pick_java_home();
    let mut java_too_old = None;
    let mut has_java = java_home.is_some();
    if let Some(home) = &java_home {
        if let Some(major) = java_major(home) {
            if major < 17 {
                has_java = false;
                java_too_old = Some((home.clone(), major));
            }
        }
    }
    if has_sdk && has_java {
        return None;
    }

    let mut missing = Vec::new();
    if sdk.emulator.is_none() {
        missing.push("Android Emulator (emulator.exe)".to_string());
    }
    if sdk.adb.is_none() {
        missing.push("Android SDK Platform-Tools (adb.exe)".to_string());
    }
    if sdk.avdmanager.is_none() {
        missing.push("Command-line tools (avdmanager)".to_string());
    }
    if !has_java {
        match &java_too_old {
            Some((_, major)) => missing.push(format!("Java JDK 17+ (yang ada cuma JDK {major})")),
            None => missing.push("Java JDK 17 (JAVA_HOME_17 / JAVA_HOME)".to_string()),
        }
    }
    if missing.is_empty() {
        return None;
    }

    Some(Preflight {
        missing,
        sdk_root: sdk.root,
        has_sdk,
        has_java,
        java_too_old,
    })
}

/// Show the pre-flight problem in a native dialog, then exit.
pub fn report_and_exit(problem: &Preflight) -> ! {
    let text = problem.message();
    let opened = std::panic::catch_unwind(|| {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("devmgr — komponen belum terpasang")
            .set_description(text.as_str())
            .set_buttons(rfd::MessageButtons::Ok)
            .show()
    });

    if opened.is_err() {
        // Last resort when no dialog can be shown (e.g. no display).
        eprintln!("devmgr: komponen belum terpasang\n\n{text}");
    }
    std::process::exit(1);
}

fn find_cmdline_tool(root: &Path, tool: &str) -> Option<PathBuf> {
    let base = root.join("cmdline-tools");
    let names = if cfg!(windows) {
        vec![format!("{tool}.bat"), format!("{tool}.cmd")]
    } else {
        vec![tool.to_string()]
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&base) {
        for e in rd.flatten() {
            dirs.push(e.path());
        }
    }
    // Prefer the versioned/latest installs, newest first, over legacy `tools`.
    dirs.sort_by(|a, b| {
        let rank = |p: &Path| {
            let n = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            if n == "latest" {
                (2, n)
            } else if n.starts_with(|c: char| c.is_ascii_digit()) {
                (1, n)
            } else {
                (0, n)
            }
        };
        rank(b).cmp(&rank(a))
    });
    for dir in dirs {
        for name in &names {
            let cand = dir.join("bin").join(name);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    which(tool)
}

/// One AVD on disk, read from its config.ini.
#[derive(Clone, Debug, Default)]
pub struct Avd {
    pub name: String,
    pub dir: PathBuf,

    pub target: String,
    pub tag: String,
    pub abi: String,
    pub device: String,
    pub ram: String,
    pub cores: String,
    pub width: String,
    pub height: String,
    pub density: String,
    pub sdcard: String,
    pub data_partition: String,
    pub gpu: String,
    pub playstore: bool,
    /// Raw config.ini lines, so unknown keys survive a rewrite.
    pub lines: Vec<String>,
}

impl Avd {
    pub fn load(ini: &Path) -> Result<Avd> {
        // The folder name is not always the AVD name (`Hape` -> `Pixel_9_Pro.avd`),
        // so the real directory comes from `path=` in the .ini.
        let ini_text = std::fs::read_to_string(ini).unwrap_or_default();
        let dir = ini_text
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once('=')?;
                (k.trim() == "path").then(|| PathBuf::from(v.trim()))
            })
            .unwrap_or_else(|| ini.with_extension("avd"));
        let cfg_path = dir.join("config.ini");
        let raw = std::fs::read_to_string(&cfg_path)
            .with_context(|| format!("read {}", cfg_path.display()))?;
        let mut avd = Avd {
            name: ini.file_stem().unwrap_or_default().to_string_lossy().to_string(),
            dir,
            lines: raw.lines().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let read = |k: &str| {
            avd.lines.iter().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == k).then(|| value.trim().to_string())
            })
        };
        avd.target = read("target").unwrap_or_default();
        avd.tag = read("tag.id").unwrap_or_default();
        avd.abi = read("abi.type").unwrap_or_default();
        avd.device = read("hw.device.name").unwrap_or_default();
        avd.ram = read("hw.ramSize").unwrap_or_default();
        avd.cores = read("hw.cpu.ncore").unwrap_or_default();
        avd.width = read("hw.lcd.width").unwrap_or_default();
        avd.height = read("hw.lcd.height").unwrap_or_default();
        avd.density = read("hw.lcd.density").unwrap_or_default();
        avd.sdcard = read("sdcard.size").unwrap_or_default();
        avd.data_partition = read("disk.dataPartition.size").unwrap_or_default();
        avd.gpu = read("hw.gpu.mode").unwrap_or_default();
        avd.playstore = read("PlayStore.enabled")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        Ok(avd)
    }

    /// Read a raw config.ini value.
    #[cfg(test)]
    fn get(&self, key: &str) -> Option<String> {
        self.lines.iter().find_map(|line| {
            let (k, v) = line.split_once('=')?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
    }

    pub fn set(&mut self, key: &str, value: &str) {
        let mut found = false;
        for line in self.lines.iter_mut() {
            if let Some((k, _)) = line.split_once('=') {
                if k.trim() == key {
                    *line = format!("{key}={value}");
                    found = true;
                }
            }
        }
        if !found {
            self.lines.push(format!("{key}={value}"));
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = self.dir.join("config.ini");
        let mut body = self.lines.join("\n");
        body.push('\n');
        std::fs::write(&path, body).with_context(|| format!("write {}", path.display()))
    }

    pub fn screen(&self) -> String {
        if self.width.is_empty() || self.height.is_empty() {
            "default".into()
        } else {
            format!("{}x{} @ {}dpi", self.width, self.height, self.density)
        }
    }

    pub fn api(&self) -> String {
        self.target.trim_start_matches("android-").to_string()
    }
}

/// A system image available locally, ready to back a new AVD.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemImage {
    pub package: String,
    pub api: String,
    pub tag: String,
    pub abi: String,
    pub path: PathBuf,
}

impl SystemImage {
    pub fn label(&self) -> String {
        let tag = match self.tag.as_str() {
            "google_apis_playstore" => "Play Store".to_string(),
            "google_apis" => "Google APIs".to_string(),
            "google_apis_playstore_tablet" => "Play Store (tablet)".to_string(),
            other => other.replace('_', " "),
        };
        format!("API {} · {} · {}", self.api, tag, self.abi)
    }
}

#[derive(Clone, Debug, Default)]
pub struct DeviceDef {
    pub id: String,
    pub name: String,
    pub oem: String,
}

impl DeviceDef {
    pub fn label(&self) -> String {
        if self.oem.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.oem)
        }
    }
}

/// One row from `adb devices -l`.
#[derive(Clone, Debug, Default)]
pub struct Device {
    pub serial: String,
    pub state: String,
    pub model: String,
    pub product: String,
    pub transport: String,
    pub is_emulator: bool,
}

pub struct AvdDraft {
    pub name: String,
    pub device_id: String,
    pub image_package: String,
    pub sdcard: String,
}

pub fn list_avds(sdk: &Sdk) -> Vec<Avd> {
    let mut out: Vec<Avd> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&sdk.avd_home) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ini") {
                continue;
            }
            if let Ok(avd) = Avd::load(&path) {
                out.push(avd);
            }
        }
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

pub fn list_images(sdk: &Sdk) -> Vec<SystemImage> {
    let mut out = Vec::new();
    let base = sdk.root.join("system-images");
    let Ok(apis) = std::fs::read_dir(&base) else {
        return out;
    };
    for api in apis.flatten() {
        let Ok(tags) = std::fs::read_dir(api.path()) else { continue };
        for tag in tags.flatten() {
            let Ok(abis) = std::fs::read_dir(tag.path()) else { continue };
            for abi in abis.flatten() {
                let dir = abi.path();
                if !dir.join("package.xml").is_file() {
                    continue;
                }
                let api_name = api.file_name().to_string_lossy().to_string();
                let tag_name = tag.file_name().to_string_lossy().to_string();
                let abi_name = abi.file_name().to_string_lossy().to_string();
                out.push(SystemImage {
                    package: format!("system-images;{api_name};{tag_name};{abi_name}"),
                    api: api_name.trim_start_matches("android-").to_string(),
                    tag: tag_name,
                    abi: abi_name,
                    path: dir,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        let key = |x: &SystemImage| {
            let api: u32 = x.api.parse().unwrap_or(0);
            (std::cmp::Reverse(api), x.tag.clone(), x.abi.clone())
        };
        key(a).cmp(&key(b))
    });
    out
}

pub fn list_device_defs(sdk: &Sdk) -> Vec<DeviceDef> {
    let Some(tool) = &sdk.avdmanager else {
        return Vec::new();
    };
    let text = proc::run_lossy(tool, &["list", "device"]);
    let mut out = Vec::new();
    let mut cur = DeviceDef::default();
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with("id:") {
            if !cur.id.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if let Some((_, rest)) = line.split_once("or") {
                cur.id = rest.trim().trim_matches('"').to_string();
            }
            continue;
        }
        let Some((k, v)) = line.trim().split_once(':') else { continue };
        let v = v.trim().to_string();
        match k.trim() {
            "Name" => cur.name = v,
            "OEM" => cur.oem = v,
            _ => {}
        }
    }
    if !cur.id.is_empty() {
        out.push(cur);
    }
    out.retain(|d| !d.id.is_empty());
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

pub fn list_devices(sdk: &Sdk) -> Vec<Device> {
    let Some(adb) = &sdk.adb else {
        return Vec::new();
    };
    let text = proc::run_lossy(adb, &["devices", "-l"]);
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('*') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let Some(serial) = parts.next() else { continue };
        let state = parts.next().unwrap_or("").to_string();
        let mut dev = Device {
            serial: serial.to_string(),
            state,
            is_emulator: serial.starts_with("emulator-"),
            ..Default::default()
        };
        for kv in parts {
            if let Some((k, v)) = kv.split_once(':') {
                match k {
                    "model" => dev.model = v.replace('_', " "),
                    "product" => dev.product = v.to_string(),
                    "transport_id" => dev.transport = v.to_string(),
                    _ => {}
                }
            }
        }
        out.push(dev);
    }
    out
}

/// Which AVD is behind an emulator serial, e.g. emulator-5554 -> "Pixel_9_Pro".
pub fn avd_name_for_serial(sdk: &Sdk, serial: &str) -> Option<String> {
    let adb = sdk.adb.as_ref()?;
    let text = proc::run_lossy(adb, &["-s", serial, "emu", "avd", "name"]);
    let name = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && *l != "OK")?;
    Some(name.to_string())
}

pub fn create_avd(sdk: &Sdk, draft: &AvdDraft) -> Result<String> {
    let tool = sdk.avdmanager.as_ref().context("avdmanager not found")?;
    let mut args: Vec<String> = vec![
        "create".into(),
        "avd".into(),
        "-n".into(),
        draft.name.clone(),
        "-k".into(),
        draft.image_package.clone(),
    ];
    if !draft.device_id.is_empty() {
        args.push("-d".into());
        args.push(draft.device_id.clone());
    }
    if !draft.sdcard.trim().is_empty() {
        args.push("--sdcard".into());
        args.push(draft.sdcard.trim().to_string());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // avdmanager asks "Do you wish to create a custom hardware profile?" — decline.
    let out = proc::run_stdin(tool, &refs, "no\nno\n")?;
    Ok(out)
}

pub fn delete_avd(sdk: &Sdk, name: &str) -> Result<String> {
    let tool = sdk.avdmanager.as_ref().context("avdmanager not found")?;
    proc::run(tool, &["delete", "avd", "-n", name])
}

/// Start an emulator detached; returns the child process.
pub fn launch(sdk: &Sdk, name: &str, cold: bool, wipe: bool, headless: bool, extra: &str) -> Result<std::process::Child> {
    let emulator = sdk.emulator.as_ref().context("emulator not found")?;
    let mut c = proc::detached(emulator);
    c.arg("-avd").arg(name);
    if cold {
        c.arg("-no-snapshot-load");
    }
    if wipe {
        c.arg("-wipe-data");
    }
    if headless {
        c.arg("-no-window");
    }
    for token in extra.split_whitespace() {
        c.arg(token);
    }
    c.spawn()
        .with_context(|| format!("start emulator {}", name))
}

pub fn kill_emulator(sdk: &Sdk, serial: &str) -> Result<String> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    proc::run(adb, &["-s", serial, "emu", "kill"])
}

pub fn logcat_tail(sdk: &Sdk, serial: &str, lines: usize, filter: &str) -> String {
    let Some(adb) = &sdk.adb else { return "adb not found".into() };
    let mut args = vec!["-s", serial, "logcat", "-d", "-t"];
    let n = lines.to_string();
    args.push(&n);
    let mut text = proc::run_lossy(adb, &args);
    if !filter.trim().is_empty() {
        let needle = filter.to_lowercase();
        text = text
            .lines()
            .filter(|l| l.to_lowercase().contains(&needle))
            .collect::<Vec<_>>()
            .join("\n");
    }
    text
}

pub fn clear_logcat(sdk: &Sdk, serial: &str) {
    if let Some(adb) = &sdk.adb {
        let _ = proc::run_lossy(adb, &["-s", serial, "logcat", "-c"]);
    }
}

pub fn screenshot(sdk: &Sdk, serial: &str) -> Result<Vec<u8>> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    proc::run_bytes(adb, &["-s", serial, "exec-out", "screencap", "-p"])
}

pub fn install_apk(sdk: &Sdk, serial: &str, apk: &Path, reinstall: bool) -> Result<String> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    let apk = apk.to_string_lossy().to_string();
    let mut args = vec!["-s", serial, "install"];
    if reinstall {
        args.push("-r");
    }
    args.push(&apk);
    proc::run(adb, &args)
}

pub fn forward(sdk: &Sdk, serial: &str, spec: &str, reverse: bool, remove: bool) -> Result<String> {
    let adb = sdk.adb.as_ref().context("adb not found")?;
    let sub = match (reverse, remove) {
        (false, false) => "forward",
        (false, true) => "forward",
        (true, false) => "reverse",
        (true, true) => "reverse",
    };
    let mut args = vec!["-s", serial, sub];
    if remove {
        args.push("--remove");
    }
    args.push(spec);
    proc::run(adb, &args)
}

pub fn list_forwards(sdk: &Sdk, serial: &str) -> String {
    let Some(adb) = &sdk.adb else { return String::new() };
    let fwd = proc::run_lossy(adb, &["-s", serial, "forward", "--list"]);
    let rev = proc::run_lossy(adb, &["-s", serial, "reverse", "--list"]);
    let mut out = String::new();
    for line in fwd.lines() {
        let line = line.trim();
        if !line.is_empty() {
            out.push_str("forward  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    for line in rev.lines() {
        let line = line.trim();
        if !line.is_empty() {
            out.push_str("reverse  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    if out.is_empty() {
        out.push_str("no port mappings\n");
    }
    out
}

pub fn list_snapshots(sdk: &Sdk, avd: &Avd) -> String {
    let Some(emulator) = &sdk.emulator else { return String::new() };
    let text = proc::run_lossy(emulator, &["-avd", &avd.name, "-snapshot-list"]);
    let mut out = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Map of AVD name -> serial for running emulators.
pub fn running_map(sdk: &Sdk, devices: &[Device]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for d in devices.iter().filter(|d| d.is_emulator) {
        if let Some(name) = avd_name_for_serial(sdk, &d.serial) {
            map.insert(name, d.serial.clone());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avd_from(lines: &[&str]) -> Avd {
        Avd {
            name: "Test".into(),
            lines: lines.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn reads_config_keys() {
        let mut avd = avd_from(&[
            "target=android-36",
            "hw.ramSize=2048",
            "PlayStore.enabled=true",
        ]);
        avd.target = "android-36".into();
        assert_eq!(avd.get("target").as_deref(), Some("android-36"));
        assert_eq!(avd.get("hw.ramSize").as_deref(), Some("2048"));
        assert_eq!(avd.api(), "36");
    }

    #[test]
    fn set_replaces_in_place_and_appends() {
        let mut avd = avd_from(&["target=android-36", "hw.ramSize=2048"]);
        avd.set("hw.ramSize", "4096");
        assert_eq!(avd.get("hw.ramSize").as_deref(), Some("4096"));
        assert_eq!(avd.lines.len(), 2);
        avd.set("hw.cpu.ncore", "8");
        assert_eq!(avd.get("hw.cpu.ncore").as_deref(), Some("8"));
        assert_eq!(avd.lines.len(), 3);
        // Unknown keys and comments survive a rewrite.
        assert_eq!(avd.lines[0], "target=android-36");
    }

    #[test]
    fn screen_falls_back_to_default() {
        let avd = avd_from(&["target=android-36"]);
        assert_eq!(avd.screen(), "default");
    }

    #[test]
    fn parses_adb_devices_l() {
        // Shape of `adb devices -l`: serial, state, then key:value pairs.
        let line = "emulator-5554          device product:sdk_gphone64 model:sdk_gphone64_x86_64 transport_id:3";
        let mut parts = line.split_whitespace();
        let serial = parts.next().unwrap();
        let state = parts.next().unwrap();
        assert_eq!(serial, "emulator-5554");
        assert_eq!(state, "device");
        assert!(serial.starts_with("emulator-"));
        let pairs: Vec<(&str, &str)> = parts.filter_map(|kv| kv.split_once(':')).collect();
        assert_eq!(pairs[1], ("model", "sdk_gphone64_x86_64"));
    }

    #[test]
    fn parses_device_def_blocks() {
        let text = "Available devices definitions:\n\
                    id: 0 or \"pixel_7_pro\"\n    Name: Pixel 7 Pro\n    OEM : Google\n    Tag : google\n\
                    ---------\n\
                    id: 1 or \"pixel_tablet\"\n    Name: Pixel Tablet\n    OEM : Google\n    Tag : android\n";
        let mut out = Vec::new();
        let mut cur = DeviceDef::default();
        for line in text.lines() {
            let line = line.trim_end();
            if line.starts_with("id:") {
                if !cur.id.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                if let Some((_, rest)) = line.split_once("or") {
                    cur.id = rest.trim().trim_matches('"').to_string();
                }
                continue;
            }
            let Some((k, v)) = line.trim().split_once(':') else { continue };
            match k.trim() {
                "Name" => cur.name = v.trim().to_string(),
                "OEM" => cur.oem = v.trim().to_string(),
                _ => {}
            }
        }
        if !cur.id.is_empty() {
            out.push(cur);
        }
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "pixel_7_pro");
        assert_eq!(out[0].label(), "Pixel 7 Pro (Google)");
        assert_eq!(out[1].id, "pixel_tablet");
    }

    /// The AVD name and its folder name often differ (`Hape` lives in `Pixel_9_Pro.avd`),
    /// so `path=` in the `.ini` must win over the file stem.
    #[test]
    fn avd_dir_comes_from_ini_path() {
        let base = std::env::temp_dir().join(format!("devmgr-test-{}", std::process::id()));
        let avd_dir = base.join("Pixel_9_Pro.avd");
        std::fs::create_dir_all(&avd_dir).unwrap();
        std::fs::write(
            avd_dir.join("config.ini"),
            "AvdId=Hape\ntarget=android-36\nhw.ramSize=2048\n",
        )
        .unwrap();
        let ini = base.join("Hape.ini");
        std::fs::write(
            &ini,
            format!("avd.ini.encoding=UTF-8\npath={}\npath.rel=avd\\Pixel_9_Pro.avd\n", avd_dir.display()),
        )
        .unwrap();

        let avd = Avd::load(&ini).unwrap();
        assert_eq!(avd.name, "Hape");
        assert_eq!(avd.dir, avd_dir);
        assert_eq!(avd.ram, "2048");
        assert_eq!(avd.api(), "36");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn system_image_package_is_canonical() {
        let img = SystemImage {
            package: "system-images;android-36;google_apis;x86_64".into(),
            api: "36".into(),
            tag: "google_apis".into(),
            abi: "x86_64".into(),
            path: PathBuf::new(),
        };
        assert_eq!(img.label(), "API 36 · Google APIs · x86_64");
    }


    /// Hits the real SDK on this machine; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn discovers_real_sdk() {
        let sdk = Sdk::discover();
        println!("root      = {}", sdk.root.display());
        println!("avd_home  = {}", sdk.avd_home.display());
        println!("emulator  = {:?}", sdk.emulator);
        println!("adb       = {:?}", sdk.adb);
        println!("avdmanager= {:?}", sdk.avdmanager);
        assert!(sdk.missing().is_empty(), "missing tools: {:?}", sdk.missing());

        let avds = list_avds(&sdk);
        println!("avds      = {}", avds.len());
        for avd in &avds {
            println!(
                "  {} api={} abi={} ram={} cores={} screen={}",
                avd.name,
                avd.api(),
                avd.abi,
                avd.ram,
                avd.cores,
                avd.screen()
            );
            assert!(!avd.abi.is_empty(), "{} has no abi", avd.name);
        }

        let images = list_images(&sdk);
        println!("images    = {}", images.len());
        for img in &images {
            println!("  {}", img.label());
            assert!(img.path.join("package.xml").is_file());
        }

        let devices = list_devices(&sdk);
        println!("devices   = {}", devices.len());
        for dev in &devices {
            println!("  {} state={} model={}", dev.serial, dev.state, dev.model);
        }

        let defs = list_device_defs(&sdk);
        println!("defs      = {}", defs.len());
        assert!(defs.len() > 10, "device definitions look empty");
        assert!(defs.iter().any(|d| d.id == "pixel_7_pro"));
    }
}
