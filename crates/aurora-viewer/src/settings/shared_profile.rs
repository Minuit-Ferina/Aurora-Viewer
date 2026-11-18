//! Both editions take the same shared lease while running. A second file
//! orders startup with lock upgrades, so no instance enters a profile in
//! the middle of bulk maintenance.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

struct Lease {
    dir: PathBuf,
    file: File,
}

static ACTIVE: OnceLock<Mutex<Option<Lease>>> = OnceLock::new();

fn file(dir: &Path, name: &str) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(name))
}

pub(crate) fn enter() -> io::Result<()> {
    let dir = super::config_dir();
    std::fs::create_dir_all(&dir)?;
    let gate = file(&dir, "profile-gate.lock")?;
    gate.lock()?;
    let usage = file(&dir, "profile-use.lock")?;
    usage.lock_shared()?;
    let _ = ACTIVE.set(Mutex::new(Some(Lease { dir, file: usage })));
    Ok(())
}

pub(crate) struct Maintenance {
    lease: Option<Lease>,
    _gate: Option<File>,
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            // The gate still excludes new launches. Restore this process's
            // shared lease before another launch can enter.
            if let Err(error) = lease.file.unlock().and_then(|()| lease.file.lock_shared()) {
                log::error!("cannot restore shared profile lease: {error}");
                return;
            }
            if let Some(state) = ACTIVE.get()
                && let Ok(mut state) = state.lock()
            {
                *state = Some(lease);
            }
        }
    }
}

/// Refuse rather than wait if the other edition or another instance is
/// active. Keep our shared lease on refusal.
pub(crate) fn try_maintenance() -> Option<Maintenance> {
    if cfg!(test) {
        return Some(Maintenance { lease: None, _gate: None });
    }
    let mut state = ACTIVE.get()?.lock().ok()?;
    let lease = state.as_ref()?;
    let gate = file(&lease.dir, "profile-gate.lock").ok()?;
    gate.try_lock().ok()?;
    lease.file.unlock().ok()?;
    if lease.file.try_lock().is_err() {
        if let Err(error) = lease.file.lock_shared() {
            log::error!("cannot restore shared profile lease: {error}");
            *state = None;
        }
        return None;
    }
    Some(Maintenance {
        lease: state.take(),
        _gate: Some(gate),
    })
}
