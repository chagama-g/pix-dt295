//! The few OS services the driver needs: its own module path, OutputDebugString and
//! local time. Windows only in practice; other targets get stand-ins so the pure
//! parts of the crate build and test anywhere.

use std::ffi::OsString;

/// Wall-clock time of day for log stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeOfDay {
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millis: u16,
}

#[cfg(windows)]
mod imp {
    use super::TimeOfDay;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::ptr;

    use windows_sys::Win32::Foundation::{HMODULE, SYSTEMTIME};
    use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW;
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        GetModuleFileNameW, GetModuleHandleExW,
    };
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;

    /// Any address inside this module (the DLL, or the test executable).
    static ANCHOR: u8 = 0;

    pub fn module_path() -> Option<OsString> {
        let mut module: HMODULE = ptr::null_mut();
        // SAFETY: with FROM_ADDRESS the "name" is an address inside the module to
        // look up (ANCHOR is a static of this module); UNCHANGED_REFCOUNT takes no
        // reference, so nothing must be released. `module` is a valid out pointer.
        let ok = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                ptr::from_ref(&ANCHOR).cast(),
                &mut module,
            )
        };
        if ok == 0 {
            return None;
        }
        let mut buf = vec![0u16; 260];
        loop {
            let cap = u32::try_from(buf.len()).ok()?;
            // SAFETY: `buf` is writable for `cap` UTF-16 units; `module` is this
            // module's handle, valid while its code runs.
            let n = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), cap) } as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                return Some(OsString::from_wide(&buf[..n]));
            }
            if buf.len() >= 32 * 1024 {
                return None;
            }
            buf.resize(buf.len() * 2, 0); // truncated: retry with a larger buffer
        }
    }

    pub fn output_debug_string(s: &str) {
        let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `w` is a NUL-terminated UTF-16 string that outlives the call.
        unsafe { OutputDebugStringW(w.as_ptr()) };
    }

    pub fn local_time() -> TimeOfDay {
        let mut t = SYSTEMTIME {
            wYear: 0,
            wMonth: 0,
            wDayOfWeek: 0,
            wDay: 0,
            wHour: 0,
            wMinute: 0,
            wSecond: 0,
            wMilliseconds: 0,
        };
        // SAFETY: `t` is a valid, writable SYSTEMTIME.
        unsafe { GetLocalTime(&mut t) };
        TimeOfDay {
            hour: t.wHour,
            minute: t.wMinute,
            second: t.wSecond,
            millis: t.wMilliseconds,
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::TimeOfDay;
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub fn module_path() -> Option<OsString> {
        None
    }

    pub fn output_debug_string(_s: &str) {}

    /// UTC (there is no portable local time in std).
    pub fn local_time() -> TimeOfDay {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let secs = d.as_secs() % 86_400;
        TimeOfDay {
            hour: (secs / 3600) as u16,
            minute: (secs / 60 % 60) as u16,
            second: (secs % 60) as u16,
            millis: d.subsec_millis() as u16,
        }
    }
}

/// Full path of the module (DLL) this code is in; `None` if unknown.
pub fn module_path() -> Option<OsString> {
    imp::module_path()
}

/// Send a line to the debugger (DebugView).
pub fn output_debug_string(s: &str) {
    imp::output_debug_string(s);
}

/// Local time of day.
pub fn local_time() -> TimeOfDay {
    imp::local_time()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_of_day_in_range() {
        let t = local_time();
        assert!(t.hour < 24 && t.minute < 60 && t.second < 61 && t.millis < 1000);
    }

    #[cfg(windows)]
    #[test]
    fn module_path_is_this_executable() {
        let p = module_path().expect("module path");
        assert_eq!(
            std::path::PathBuf::from(p).canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
    }
}
