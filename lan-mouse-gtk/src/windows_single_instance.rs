//! Single frontend instance on Windows.
//!
//! The first instance owns the named mutex `Local\LanMouseFrontend` and a
//! named auto-reset event `Local\LanMouseShow`. A later instance finds the
//! mutex taken, signals the event and exits; the first instance waits on the
//! event in a thread and shows its window.

use std::{
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0},
    System::Threading::{
        CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
        WaitForSingleObject,
    },
};

const MUTEX_NAME: &str = "Local\\LanMouseFrontend";
const EVENT_NAME: &str = "Local\\LanMouseShow";

/// Event handle of the first instance (0 = none). Handles are kept open for
/// the lifetime of the process on purpose: the mutex must stay owned.
static SHOW_EVENT: AtomicUsize = AtomicUsize::new(0);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Returns `true` if this process is the first (or the check failed and we
/// continue anyway), `false` if another instance runs and was asked to show
/// its window.
pub fn acquire() -> bool {
    let mutex_name = wide(MUTEX_NAME);
    let event_name = wide(EVENT_NAME);
    // SAFETY: valid NUL-terminated wide strings, null security attributes.
    unsafe {
        let mutex = CreateMutexW(ptr::null(), 0, mutex_name.as_ptr());
        if mutex.is_null() {
            log::warn!("could not create single instance mutex: {}", GetLastError());
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(mutex);
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, event_name.as_ptr());
            if event.is_null() {
                log::warn!("could not open show event: {}", GetLastError());
            } else {
                if SetEvent(event) == 0 {
                    log::warn!("could not signal show event: {}", GetLastError());
                }
                CloseHandle(event);
            }
            return false;
        }
        // auto-reset, initially non-signaled
        let event = CreateEventW(ptr::null(), 0, 0, event_name.as_ptr());
        if event.is_null() {
            log::warn!("could not create show event: {}", GetLastError());
        } else {
            SHOW_EVENT.store(event as usize, Ordering::SeqCst);
        }
    }
    true
}

/// Calls `on_show` (from a background thread) every time another instance
/// asks this one to show its window. No-op if the event does not exist.
pub fn listen_for_show<F: Fn() + Send + 'static>(on_show: F) {
    let event = SHOW_EVENT.load(Ordering::SeqCst);
    if event == 0 {
        return;
    }
    let spawned = thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || {
            let event = event as HANDLE;
            // SAFETY: the handle is never closed while the process runs.
            while unsafe { WaitForSingleObject(event, INFINITE) } == WAIT_OBJECT_0 {
                on_show();
            }
            log::warn!("single instance listener stopped");
        });
    if let Err(e) = spawned {
        log::warn!("could not start single instance listener: {e}");
    }
}
