//! The AppKit side of the session: the main-thread run loop and the user's own input to the
//! apps it controls, like the original's overlay. Unlike the original, Esc never stops the
//! agent: an Esc meant for any other app would stop every agent session at once.
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::*;
use std::sync::Mutex;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventGetIntegerValueField(e: *const std::ffi::c_void, field: u32) -> i64;
}
use std::time::Instant;

/// The user's last key press, click or scroll per app (pid).
static TOUCHED: Mutex<Vec<(i32, Instant)>> = Mutex::new(Vec::new());

/// Run AppKit on the main thread (never returns); `work` runs the MCP loop on another thread.
pub fn run(work: impl FnOnce() + Send + 'static) -> ! {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory); // no Dock icon, never frontmost
    let input = RcBlock::new(|e: std::ptr::NonNull<NSEvent>| {
        let e = unsafe { e.as_ref() };
        let key = e.r#type() == NSEventType::KeyDown;
        // Keys this process posted (field 41, kCGEventSourceUnixProcessID) are the agent's own.
        let cg: *const std::ffi::c_void = unsafe { objc2::msg_send![e, CGEvent] };
        if !cg.is_null() && unsafe { CGEventGetIntegerValueField(cg, 41) } == std::process::id() as i64 {
            return;
        }
        // Keys go to the frontmost app, pointer events to the app owning the window under them.
        let pid = if key { crate::apps::frontmost() } else { crate::window_owner(e.windowNumber() as u32) };
        if let Some(pid) = pid {
            let mut touched = TOUCHED.lock().unwrap();
            touched.retain(|(p, _)| *p != pid);
            touched.push((pid, Instant::now()));
        }
    });
    // Global monitors see other apps' input once the process is trusted for Accessibility. They
    // don't see the events this process posts to an app, so only the user's input counts.
    let mask = NSEventMask::KeyDown | NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown
        | NSEventMask::OtherMouseDown | NSEventMask::ScrollWheel;
    std::mem::forget(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &input));
    std::thread::spawn(move || {
        work();
        std::process::exit(0);
    });
    app.run();
    std::process::exit(0)
}

/// When the user last pressed a key, clicked or scrolled in the app.
pub fn touched(pid: i32) -> Option<Instant> {
    TOUCHED.lock().unwrap().iter().find(|(p, _)| *p == pid).map(|(_, t)| *t)
}
