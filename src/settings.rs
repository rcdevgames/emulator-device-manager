use std::path::PathBuf;

/// User preferences persisted next to the user profile.
#[derive(Clone, Default)]
pub struct Settings {
    pub sdk_override: String,
    pub extra_args: String,
    pub live_ms: u64,
    /// WSA install directory; empty means auto-detect.
    pub wsa_root: String,
    /// Kicked off on launch so the WSA agent is usable without opening any app.
    pub wsa_autostart: bool,
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("devmgr").join("settings.ini"))
}

impl Settings {
    pub fn load() -> Settings {
        let mut s = Settings {
            live_ms: 900,
            wsa_autostart: true,
            ..Default::default()
        };
        let Some(path) = config_path() else { return s };
        let Ok(raw) = std::fs::read_to_string(path) else { return s };
        for line in raw.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim().to_string();
            match k.trim() {
                "sdk_override" => s.sdk_override = v,
                "extra_args" => s.extra_args = v,
                "live_ms" => s.live_ms = v.parse().unwrap_or(900).clamp(250, 5000),
                "wsa_root" => s.wsa_root = v,
                "wsa_autostart" => s.wsa_autostart = v != "0",
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let body = format!(
            "sdk_override={}\nextra_args={}\nlive_ms={}\nwsa_root={}\nwsa_autostart={}\n",
            self.sdk_override,
            self.extra_args,
            self.live_ms,
            self.wsa_root,
            if self.wsa_autostart { 1 } else { 0 }
        );
        let _ = std::fs::write(path, body);
    }
}
