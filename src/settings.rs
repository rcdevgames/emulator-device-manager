use std::path::PathBuf;

/// User preferences persisted next to the user profile.
#[derive(Clone, Default)]
pub struct Settings {
    pub sdk_override: String,
    pub extra_args: String,
    pub live_ms: u64,
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
            "sdk_override={}\nextra_args={}\nlive_ms={}\n",
            self.sdk_override, self.extra_args, self.live_ms
        );
        let _ = std::fs::write(path, body);
    }
}
