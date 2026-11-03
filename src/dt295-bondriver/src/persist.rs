//! What the driver learns at run time and needs again next time, kept in the state
//! file ([`dt295::state`]).
//!
//! Why each record exists:
//!  - linkkey: the device link-encrypts the TS with the first key it handed out
//!    after power-up; a re-open without a re-plug needs that key.
//!  - psi: satellite PAT/PMT/SDT/NIT sections, re-sent while a TS delivers none (an
//!    uncontracted pay service), so a scan still lists it. Unseen for 90 days =
//!    dropped (by the core, on import).

use std::path::PathBuf;

use dt295::state::State;
use dt295::{Device, PsiRecord};
use tracing::{info, warn};

/// The host-side copy of the persistent state and the rules for writing it.
#[derive(Debug)]
pub struct Persist {
    /// `None`: nothing is read or written (tests only; an ini always names a file).
    file: Option<PathBuf>,
    /// The link key or the PSI changed since the last save.
    dirty: bool,
    link_key: Option<[u8; 16]>,
    /// Keys the device handed out to processes that ended before proving one (written
    /// by `dt295 capture`); tried after `link_key`. Once a stream proves a key, only
    /// the key this open was handed stays, while it is not the proved one.
    pending_keys: Vec<[u8; 16]>,
    /// PSI as last exported from the core (or loaded).
    psi: Vec<PsiRecord>,
}

impl Persist {
    pub fn new(file: Option<PathBuf>) -> Self {
        Self {
            file,
            dirty: false,
            link_key: None,
            pending_keys: Vec::new(),
            psi: Vec::new(),
        }
    }

    /// `LoadState`: the link key and the PSI cache from the state file.
    pub fn load(&mut self) {
        let Some(file) = self.file.clone() else {
            info!("state file: disabled");
            return;
        };
        let st = match State::load(&file) {
            Ok(Some(st)) => st,
            Ok(None) => {
                info!("state: none yet at {}", file.display());
                return;
            }
            Err(e) => {
                warn!("state: cannot read {} ({e})", file.display());
                return;
            }
        };
        self.pending_keys = st.pending_keys.clone();
        if st.link_key.is_some() {
            self.link_key = st.link_key;
        }
        self.psi = st.psi;
        info!(
            "state: link key {}, {} PSI section(s) from {}",
            yes_no(self.link_key.is_some()),
            self.psi.len(),
            file.display()
        );
    }

    /// `ApplyState`, before [`Device::start`]: the link key (a cold boot discards
    /// it) and the PSI cache.
    pub fn apply(&mut self, dev: &mut Device) {
        let keys: Vec<[u8; 16]> = self
            .link_key
            .into_iter()
            .chain(self.pending_keys.iter().copied())
            .collect();
        if !keys.is_empty() {
            dev.set_prior_link_keys(&keys);
            info!(
                "link key: handed the previous session's key{} to the core",
                if self.pending_keys.is_empty() {
                    ""
                } else {
                    " and the unproved ones"
                }
            );
        }
        let kept = self.psi.iter().filter(|r| dev.psi_import(r)).count();
        if kept != self.psi.len() {
            self.dirty = true; // some aged out
        }
        if self.dirty || !self.psi.is_empty() {
            self.psi = dev.psi_export(); // the core now holds them
        }
    }

    /// `SnapshotPsi`, after a stream stopped (the core's cache is not being written
    /// then): take its PSI if it changed.
    pub fn snapshot_psi(&mut self, dev: &mut Device) {
        if dev.psi_changed() {
            self.psi = dev.psi_export();
            self.dirty = true;
        }
    }

    /// `SaveState`: write the state if anything changed. Safe while streaming: the
    /// PSI comes from the last snapshot, the link key from the device's memory.
    pub fn save(&mut self, dev: Option<&Device>) {
        let Some(file) = &self.file else { return };
        if let Some((dev, key)) = dev.and_then(|d| d.link_key().map(|k| (d, k))) {
            // the unproved keys give way to the one this open was handed, while not the
            // proved one: the device may still switch to it
            let unproved: Vec<[u8; 16]> = dev.unproved_link_key().into_iter().collect();
            if self.link_key != Some(key) || self.pending_keys != unproved {
                self.link_key = Some(key);
                self.pending_keys = unproved;
                self.dirty = true;
                info!("link key: a stream proved a new key; saving it for the next open");
            }
        }
        if !self.dirty {
            return;
        }
        let st = State {
            link_key: self.link_key,
            pending_keys: self.pending_keys.clone(),
            psi: std::mem::take(&mut self.psi),
        };
        let result = st.save(file);
        self.psi = st.psi;
        if let Err(e) = result {
            warn!("state: write failed; keeping the previous file ({e})");
            return;
        }
        self.dirty = false;
        info!(
            "state: saved (link key {}, {} PSI section(s))",
            yes_no(self.link_key.is_some()),
            self.psi.len()
        );
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dt295-bon-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn psi_rec(seen_day: u32) -> PsiRecord {
        PsiRecord {
            kind: 1,
            other: false,
            id: 0x40F2,
            sub: 0,
            seen_day,
            section: vec![0, 0xB0, 0x0D, 0x40, 0xF2],
        }
    }

    #[test]
    fn disabled_reads_and_writes_nothing() {
        let dir = scratch_dir("disabled");
        let mut p = Persist::new(None);
        p.load();
        p.dirty = true;
        p.save(None);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_and_save() {
        let dir = scratch_dir("load");
        let file = dir.join("BonDriver_DT295.state");
        State {
            link_key: Some([7; 16]),
            pending_keys: Vec::new(),
            psi: vec![psi_rec(20_000)],
        }
        .save(&file)
        .unwrap();

        let mut p = Persist::new(Some(file.clone()));
        p.load();
        assert_eq!(p.link_key, Some([7; 16]));
        assert_eq!(p.psi, vec![psi_rec(20_000)]);
        // Nothing changed: no write (the file is not recreated once removed).
        fs::remove_file(&file).unwrap();
        p.save(None);
        assert!(!file.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_state_file_yet() {
        let dir = scratch_dir("fresh");
        let file = dir.join("BonDriver_DT295.state");
        let mut p = Persist::new(Some(file.clone()));
        p.load();
        assert!(!p.dirty && p.link_key.is_none() && p.psi.is_empty());
        p.save(None);
        assert!(!file.exists(), "nothing learned yet: nothing written");
        let _ = fs::remove_dir_all(&dir);
    }
}
