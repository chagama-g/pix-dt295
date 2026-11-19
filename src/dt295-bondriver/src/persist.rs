//! What the driver learns at run time and needs again next time, kept in the state
//! file ([`dt295::state`], whose rules for the link keys and the PSI cache the
//! `dt295` command follows too).
//!
//! Why each record exists:
//!  - linkkey: the device link-encrypts the TS with the first key it handed out
//!    after power-up; a re-open without a re-plug needs that key.
//!  - psi: satellite PAT/PMT/SDT/NIT sections, re-sent while a TS delivers none (an
//!    uncontracted pay service), so a scan still lists it. Unseen for 90 days =
//!    dropped (by the core, on import).

use std::path::PathBuf;

use dt295::Device;
use dt295::state::State;
use tracing::{debug, info, warn};

/// The host-side copy of the persistent state and the rules for writing it.
#[derive(Debug)]
pub struct Persist {
    /// `None`: nothing is read or written (tests only; an ini always names a file).
    file: Option<PathBuf>,
    /// The link key or the PSI changed since the last save.
    dirty: bool,
    /// The file's records: the link key and the unproved keys (written by `dt295
    /// capture` and `dt295 serve`, which keep a key the device hands out at once; tried
    /// after the link key), the PSI as last exported from the core (or loaded), and in
    /// raw-TS mode the last-viewed (space, channel), to prime on at the next open.
    st: State,
    /// The file is there but could not be read: it is left alone (nothing is written)
    /// until a load reads it, as a write would lose its link keys and PSI.
    unreadable: bool,
    /// The state went to a device since the last load: another program may have written
    /// the file since ([`Persist::reload`]).
    handed: bool,
}

impl Persist {
    pub fn new(file: Option<PathBuf>) -> Self {
        Self {
            file,
            dirty: false,
            st: State::default(),
            unreadable: false,
            handed: false,
        }
    }

    /// `LoadState`: the link key and the PSI cache from the state file.
    pub fn load(&mut self) {
        let Some(file) = self.file.clone() else {
            info!("state file: disabled");
            return;
        };
        self.handed = false;
        let st = match State::load_settled(&file) {
            Ok(Some(st)) => st,
            Ok(None) => {
                // none yet, or removed: what this driver holds (if anything) is rebuilt
                info!("state: none yet at {}", file.display());
                self.unreadable = false;
                return;
            }
            Err(e) => {
                warn!(
                    "state: cannot read {} ({e}); leaving it as it is, nothing is saved until \
                     it reads",
                    file.display()
                );
                self.unreadable = true;
                return;
            }
        };
        self.st = st;
        self.unreadable = false;
        info!(
            "state: link key {}, {} PSI section(s) from {}",
            yes_no(self.st.link_key.is_some()),
            self.st.psi.len(),
            file.display()
        );
    }

    /// `LoadState` again at an `OpenTuner` once the state went to a device: while this
    /// driver had the tuner closed, another program (`dt295 capture` with this ini, another
    /// host of the DLL) may have written the file, with a link key the device now applies.
    /// Not over changes this one could not save yet.
    pub fn reload(&mut self) {
        if self.handed && !self.dirty {
            self.load();
        }
    }

    /// `ApplyState`, before [`Device::start`]: the link key (a cold boot discards
    /// it) and the PSI cache.
    pub fn apply(&mut self, dev: &mut Device) {
        self.handed = true;
        let keys = self.st.prior_keys();
        if !keys.is_empty() {
            dev.set_prior_link_keys(&keys);
            info!(
                "link key: handed the previous session's key{} to the core",
                if self.st.pending_keys.is_empty() {
                    ""
                } else {
                    " and the unproved ones"
                }
            );
        }
        if self.st.import_psi(dev) {
            self.dirty = true; // some aged out
        }
    }

    /// `SnapshotPsi`, after a stream stopped (the core's cache is not being written
    /// then): take its PSI if it changed.
    pub fn snapshot_psi(&mut self, dev: &mut Device) {
        if dev.psi_changed() {
            self.st.psi = dev.psi_export();
            self.dirty = true;
        }
    }

    /// `SaveState`: write the state if anything changed. Safe while streaming: the
    /// PSI comes from the last snapshot, the link key from the device's memory.
    pub fn save(&mut self, dev: Option<&Device>) {
        let Some(file) = &self.file else { return };
        if let Some(dev) = dev
            && self
                .st
                .take_link_keys(dev.link_key(), dev.unproved_link_key())
        {
            self.dirty = true;
            info!("link key: a stream proved a new key; saving it for the next open");
        }
        if !self.dirty {
            return;
        }
        if self.unreadable {
            debug!(
                "state: {} could not be read; not writing over it",
                file.display()
            );
            return;
        }
        if let Err(e) = self.st.save(file) {
            warn!("state: write failed; keeping the previous file ({e})");
            return;
        }
        self.dirty = false;
        info!(
            "state: saved (link key {}, {} PSI section(s))",
            yes_no(self.st.link_key.is_some()),
            self.st.psi.len()
        );
    }

    /// Raw-TS: remember the last-viewed `(space, channel)` to prime on at the next open, and
    /// write the state file. Writes without reading the device (`save(None)`), so it is safe
    /// while a raw worker owns it, and keeps the rest of the state (link key, PSI).
    pub fn save_raw_channel(&mut self, space: u32, channel: u32) {
        if self.st.raw_channel == Some((space, channel)) {
            return;
        }
        self.st.raw_channel = Some((space, channel));
        self.dirty = true;
        self.save(None);
    }

    /// The last-viewed raw-TS `(space, channel)`, if the state file had one.
    pub fn raw_channel(&self) -> Option<(u32, u32)> {
        self.st.raw_channel
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dt295::PsiRecord;
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
        let saved = State {
            link_key: Some([7; 16]),
            pending_keys: vec![[8; 16]],
            psi: vec![psi_rec(20_000)],
            raw_channel: Some((3, 4)),
        };
        saved.save(&file).unwrap();

        let mut p = Persist::new(Some(file.clone()));
        p.load();
        assert_eq!(p.st, saved, "every record");
        assert_eq!(p.raw_channel(), Some((3, 4)));
        // Nothing changed: no write (the file is not recreated once removed).
        fs::remove_file(&file).unwrap();
        p.save(None);
        assert!(!file.exists());
        // A new raw channel is written at once, with the rest of the state.
        p.save_raw_channel(1, 2);
        let st = State::load(&file).unwrap().unwrap();
        assert_eq!(st.raw_channel, Some((1, 2)));
        assert_eq!(
            (st.link_key, st.pending_keys),
            (Some([7; 16]), vec![[8; 16]])
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_file_is_left_alone() {
        // a folder where the file goes: there, but not readable as a file
        let dir = scratch_dir("unreadable");
        let file = dir.join("BonDriver_DT295.state");
        fs::create_dir(&file).unwrap();
        let mut p = Persist::new(Some(file.clone()));
        p.load();
        assert!(p.unreadable);
        p.save_raw_channel(1, 2); // a change to save
        assert!(file.is_dir(), "not written over");
        // once it reads, it is the state again and saving works
        fs::remove_dir(&file).unwrap();
        State::default().save(&file).unwrap();
        p.load();
        assert!(!p.unreadable);
        p.save_raw_channel(3, 4);
        assert_eq!(
            State::load(&file).unwrap().unwrap().raw_channel,
            Some((3, 4))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reload_takes_what_another_program_wrote() {
        let dir = scratch_dir("reload");
        let file = dir.join("BonDriver_DT295.state");
        let mine = State {
            link_key: Some([1; 16]),
            ..State::default()
        };
        mine.save(&file).unwrap();
        let mut p = Persist::new(Some(file.clone()));
        p.load();
        // dt295 capture proves another key while this driver has the tuner closed
        let theirs = State {
            link_key: Some([2; 16]),
            ..State::default()
        };
        theirs.save(&file).unwrap();
        p.reload();
        assert_eq!(p.st, mine, "not handed to a device since the load: kept");
        p.handed = true; // as by apply
        p.dirty = true;
        p.reload();
        assert_eq!(p.st, mine, "unsaved changes: kept");
        p.dirty = false;
        p.reload();
        assert_eq!(p.st, theirs);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_state_file_yet() {
        let dir = scratch_dir("fresh");
        let file = dir.join("BonDriver_DT295.state");
        let mut p = Persist::new(Some(file.clone()));
        p.load();
        assert!(!p.dirty && p.st == State::default());
        p.save(None);
        assert!(!file.exists(), "nothing learned yet: nothing written");
        let _ = fs::remove_dir_all(&dir);
    }
}
