//! Hold the display awake only for a running video session. The owning playback
//! worker releases its native assertion on pause, EOF, error, stop or shutdown.
#[derive(Default)]
pub(super) struct DisplaySleep {
    requested: bool,
    token: Option<platform::Token>,
}

impl DisplaySleep {
    pub(super) fn update(&mut self, playing_video: bool) {
        if self.requested == playing_video {
            return;
        }
        self.requested = playing_video;
        self.token = if playing_video {
            let token = platform::Token::acquire();
            if token.is_none() {
                eprintln!("Unable to prevent display sleep during video playback");
            }
            token
        } else {
            None
        };
    }
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };

    pub(super) struct Token;
    impl Token {
        pub(super) fn acquire() -> Option<Self> {
            // Thread scoped: acquire and release both run on the playback worker.
            let previous = unsafe {
                SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED)
            };
            if previous != 0 { Some(Self) } else { None }
        }
    }
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                SetThreadExecutionState(ES_CONTINUOUS);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{c_char, c_void};
    type CFString = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> CFString;
        fn CFRelease(value: *const c_void);
    }
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CFString,
            level: u32,
            reason: CFString,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
        #[cfg(test)]
        fn IOPMAssertionCopyProperties(id: u32) -> *const c_void;
    }

    pub(super) struct Token(u32);
    impl Token {
        pub(super) fn acquire() -> Option<Self> {
            unsafe {
                const UTF8: u32 = 0x0800_0100;
                let kind = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"PreventUserIdleDisplaySleep".as_ptr(),
                    UTF8,
                );
                let reason = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"Pordo video playback".as_ptr(),
                    UTF8,
                );
                let mut id = 0;
                let ok = !kind.is_null()
                    && !reason.is_null()
                    && IOPMAssertionCreateWithName(kind, 1, reason, &mut id) == 0;
                if !kind.is_null() {
                    CFRelease(kind);
                }
                if !reason.is_null() {
                    CFRelease(reason);
                }
                if ok { Some(Self(id)) } else { None }
            }
        }
        #[cfg(test)]
        pub(super) fn id(&self) -> u32 {
            self.0
        }
    }
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                IOPMAssertionRelease(self.0);
            }
        }
    }
    #[cfg(test)]
    pub(super) fn assertion_exists(id: u32) -> bool {
        unsafe {
            let properties = IOPMAssertionCopyProperties(id);
            if properties.is_null() {
                return false;
            }
            CFRelease(properties);
            true
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    pub(super) struct Token;
    impl Token {
        pub(super) fn acquire() -> Option<Self> {
            Some(Self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Exercises the native display-sleep assertion on the current host"]
    fn native_video_assertion_can_be_acquired_reused_and_released() {
        let mut guard = DisplaySleep::default();
        guard.update(false);
        assert!(guard.token.is_none());
        guard.update(true);
        assert!(guard.token.is_some());
        #[cfg(target_os = "macos")]
        let id = guard.token.as_ref().unwrap().id();
        #[cfg(target_os = "macos")]
        assert!(platform::assertion_exists(id));
        guard.update(true);
        assert!(guard.token.is_some());
        guard.update(false);
        assert!(guard.token.is_none());
        #[cfg(target_os = "macos")]
        assert!(!platform::assertion_exists(id));
        guard.update(true);
        #[cfg(target_os = "macos")]
        let id = guard.token.as_ref().unwrap().id();
        drop(guard);
        #[cfg(target_os = "macos")]
        assert!(!platform::assertion_exists(id));
    }
}
