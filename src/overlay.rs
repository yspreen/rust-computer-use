//! The AppKit side of the session: the main-thread run loop, and Esc to stop the agent, like
//! the original's overlay.
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::*;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Last agent action, and when the user pressed Esc during a session.
static ACTIVE: Mutex<Option<Instant>> = Mutex::new(None);
static STOPPED: Mutex<Option<Instant>> = Mutex::new(None);
const SESSION_IDLE: Duration = Duration::from_secs(30);

/// Run AppKit on the main thread (never returns); `work` runs the MCP loop on another thread.
pub fn run(work: impl FnOnce() + Send + 'static) -> ! {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory); // no Dock icon, never frontmost
    let esc = RcBlock::new(|e: std::ptr::NonNull<NSEvent>| {
        let recent = ACTIVE.lock().unwrap().is_some_and(|t| t.elapsed() < SESSION_IDLE);
        if unsafe { e.as_ref() }.keyCode() == 53 && recent {
            *STOPPED.lock().unwrap() = Some(Instant::now());
            crate::cursor::hide();
        }
    });
    // Global monitors see other apps' keys once the process is trusted for Accessibility.
    std::mem::forget(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &esc));
    std::thread::spawn(move || {
        work();
        std::process::exit(0);
    });
    app.run();
    std::process::exit(0)
}

/// True once the user pressed Esc, until the agent has been idle for a whole turn.
pub fn stopped() -> bool {
    let mut s = STOPPED.lock().unwrap();
    let idle = ACTIVE.lock().unwrap().is_none_or(|t| t.elapsed() >= SESSION_IDLE);
    if idle {
        *s = None;
    }
    *ACTIVE.lock().unwrap() = Some(Instant::now());
    s.is_some()
}
