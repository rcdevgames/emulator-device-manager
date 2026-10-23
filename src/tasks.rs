use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::sdk::{Avd, Device, DeviceDef, SystemImage};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Err,
}

pub enum Msg {
    Idle,
    Log(Level, String),
    Avds(Vec<Avd>),
    Devices(Vec<Device>),
    Running(BTreeMap<String, String>),
    Images(Vec<SystemImage>),
    Defs(Vec<DeviceDef>),
    Screen(Vec<u8>),
    RecordStarted {
        serial: String,
        remote_path: String,
        pid: u32,
    },
    RecordStartFailed(String),
    RecordStopped(PathBuf),
    RecordStopFailed {
        serial: String,
        remote_path: String,
        pid: u32,
        error: String,
    },
    Logcat(String),
    Forwards(String),
    Snapshots(String),
    /// Completion signals for the self-scheduling workers in `app.rs`.
    Refreshed,
    Polled,
    Lived,
    /// Ask the UI thread for a full reload (sent by long tasks).
    Refresh,
}

/// Channel shared with worker threads. The owner must count one pending task
/// per `spawn`; every worker reports `Idle` when it is done.
pub struct Tasks {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Tasks {
    pub fn new() -> Tasks {
        let (tx, rx) = channel();
        Tasks { tx, rx }
    }

    pub fn drain(&self) -> Vec<Msg> {
        self.rx.try_iter().collect()
    }

    pub fn spawn<F>(&self, f: F)
    where
        F: FnOnce(Sender<Msg>) + Send + 'static,
    {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let guard = IdleOnDrop(tx.clone());
            f(tx);
            drop(guard);
        });
    }
}

struct IdleOnDrop(Sender<Msg>);

impl Drop for IdleOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(Msg::Idle);
    }
}
