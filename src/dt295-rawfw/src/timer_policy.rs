//! Keep Windows 11 from ignoring a stream's timer request while its host is minimized.
//! The policy is process-wide. Streams in this loaded driver share a reference count;
//! the last one restores only the timer-policy bit, preserving other policy changes.

use std::sync::Mutex;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

const IGNORE_TIMER_RESOLUTION: u32 = 4;
const PROCESS_POWER_THROTTLING: u32 = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    version: u32,
    control: u32,
    enabled: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetProcessInformation"]
    fn get_process_information(
        process: *mut std::ffi::c_void,
        class: u32,
        info: *mut State,
        size: u32,
    ) -> i32;
    #[link_name = "SetProcessInformation"]
    fn set_process_information(
        process: *mut std::ffi::c_void,
        class: u32,
        info: *const State,
        size: u32,
    ) -> i32;
}

trait PolicyApi {
    fn read(&mut self) -> Result<State, String>;
    fn write(&mut self, state: State) -> Result<(), String>;
}

struct WindowsPolicy;

impl PolicyApi for WindowsPolicy {
    fn read(&mut self) -> Result<State, String> {
        // GetProcessInformation requires the input Version even though it returns a state.
        let mut state = State { version: 1, control: 0, enabled: 0 };
        // SAFETY: the current-process pseudo handle is valid, and state is a writable,
        // correctly initialized PROCESS_POWER_THROTTLING_STATE with the supplied size.
        if unsafe {
            get_process_information(GetCurrentProcess(), PROCESS_POWER_THROTTLING,
                                    &raw mut state, size_of::<State>() as u32)
        } == 0 {
            Err(format!("GetProcessInformation: {}", std::io::Error::last_os_error()))
        } else {
            Ok(state)
        }
    }

    fn write(&mut self, state: State) -> Result<(), String> {
        // SAFETY: state has the documented layout/version and lives across the call;
        // the pseudo handle refers to this process, whose policy we may change.
        if unsafe {
            set_process_information(GetCurrentProcess(), PROCESS_POWER_THROTTLING,
                                    &raw const state, size_of::<State>() as u32)
        } == 0 {
            Err(format!("SetProcessInformation: {}", std::io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Requests {
    users: usize,
    original: Option<State>,
}

impl Requests {
    fn acquire(&mut self, api: &mut impl PolicyApi) -> Result<(), String> {
        if self.users == 0 {
            let original = api.read()?;
            let mut honored = original;
            honored.control |= IGNORE_TIMER_RESOLUTION;
            honored.enabled &= !IGNORE_TIMER_RESOLUTION;
            api.write(honored)?;
            self.original = Some(original);
        }
        self.users += 1;
        Ok(())
    }

    fn release(&mut self, api: &mut impl PolicyApi) -> Result<bool, String> {
        self.users -= 1;
        if self.users != 0 {
            return Ok(false);
        }
        let original = self.original.take().expect("a live policy request has an original");
        let mut current = api.read()?;
        // Another component may have deliberately changed our bit. Do not overwrite it.
        if current.control & IGNORE_TIMER_RESOLUTION == 0
            || current.enabled & IGNORE_TIMER_RESOLUTION != 0
        {
            return Ok(false);
        }
        current.control = (current.control & !IGNORE_TIMER_RESOLUTION)
            | (original.control & IGNORE_TIMER_RESOLUTION);
        current.enabled = (current.enabled & !IGNORE_TIMER_RESOLUTION)
            | (original.enabled & IGNORE_TIMER_RESOLUTION);
        api.write(current)?;
        Ok(true)
    }
}

static REQUESTS: Mutex<Requests> = Mutex::new(Requests { users: 0, original: None });

#[derive(Debug)]
pub(crate) struct StreamTimerPolicy {
    _private: (),
}

impl StreamTimerPolicy {
    pub(crate) fn request() -> Result<Self, String> {
        let mut requests = REQUESTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        requests.acquire(&mut WindowsPolicy)?;
        Ok(Self { _private: () })
    }
}

impl Drop for StreamTimerPolicy {
    fn drop(&mut self) {
        let mut requests = REQUESTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match requests.release(&mut WindowsPolicy) {
            Ok(true) => tracing::debug!("raw-TS cold: restored the timer power policy"),
            Ok(false) => {},
            Err(why) => tracing::warn!("raw-TS cold: could not restore timer power policy: {why}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakePolicy {
        state: State,
        writes: Vec<State>,
        fail_read: bool,
        fail_write: bool,
    }

    impl PolicyApi for FakePolicy {
        fn read(&mut self) -> Result<State, String> {
            if self.fail_read { Err("read failed".into()) } else { Ok(self.state) }
        }
        fn write(&mut self, state: State) -> Result<(), String> {
            if self.fail_write { return Err("write failed".into()); }
            self.state = state;
            self.writes.push(state);
            Ok(())
        }
    }

    fn setup(control: u32, enabled: u32) -> (Requests, FakePolicy) {
        (Requests { users: 0, original: None }, FakePolicy {
            state: State { version: 1, control, enabled }, writes: Vec::new(),
            fail_read: false, fail_write: false,
        })
    }

    #[test]
    fn overlapping_streams_restore_only_after_the_last_release() {
        let (mut requests, mut api) = setup(0, 0);
        let original = api.state;
        requests.acquire(&mut api).unwrap();
        requests.acquire(&mut api).unwrap();
        assert_eq!(api.writes.len(), 1);
        assert!(!requests.release(&mut api).unwrap());
        assert_eq!(api.state.control, IGNORE_TIMER_RESOLUTION);
        assert!(requests.release(&mut api).unwrap());
        assert_eq!(api.state, original);
    }

    #[test]
    fn restores_an_explicit_original_policy_and_preserves_other_changes() {
        let (mut requests, mut api) = setup(4, 4);
        requests.acquire(&mut api).unwrap();
        api.state.control |= 1;
        api.state.enabled |= 1;
        requests.release(&mut api).unwrap();
        assert_eq!((api.state.control, api.state.enabled), (5, 5));
    }

    #[test]
    fn does_not_overwrite_an_external_change_to_the_timer_bit() {
        let (mut requests, mut api) = setup(0, 0);
        requests.acquire(&mut api).unwrap();
        api.state.enabled |= 4;
        assert!(!requests.release(&mut api).unwrap());
        assert_eq!(api.state.enabled, 4);
    }

    #[test]
    fn a_failed_request_does_not_own_a_release() {
        let (mut requests, mut api) = setup(0, 0);
        api.fail_read = true;
        assert!(requests.acquire(&mut api).is_err());
        assert_eq!(requests.users, 0);
        api.fail_read = false;
        api.fail_write = true;
        assert!(requests.acquire(&mut api).is_err());
        assert_eq!(requests.users, 0);
        assert!(requests.original.is_none());
        api.fail_write = false;
        requests.acquire(&mut api).unwrap();
        requests.release(&mut api).unwrap();
        assert_eq!(api.state.control, 0);
    }
}
