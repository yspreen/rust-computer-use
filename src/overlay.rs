//! The AppKit side of the session: the main-thread run loop, Esc to stop the agent, the user's
//! own input to the apps it controls, and keeping those apps from taking the front, like the
//! original's overlay and focus-steal preventer.
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::*;
use objc2_foundation::NSNotification;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Last agent action, and when the user pressed Esc during a session.
static ACTIVE: Mutex<Option<Instant>> = Mutex::new(None);
static STOPPED: Mutex<Option<Instant>> = Mutex::new(None);
/// The user's last key press, click or scroll per app (pid).
static TOUCHED: Mutex<Vec<(i32, Instant)>> = Mutex::new(Vec::new());
/// The app the agent acts in, and the app that was frontmost when it started.
static GUARD: Mutex<Option<(i32, i32)>> = Mutex::new(None);
const SESSION_IDLE: Duration = Duration::from_secs(30);

/// Run AppKit on the main thread (never returns); `work` runs the MCP loop on another thread.
pub fn run(work: impl FnOnce() + Send + 'static) -> ! {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory); // no Dock icon, never frontmost
    let input = RcBlock::new(|e: std::ptr::NonNull<NSEvent>| {
        let e = unsafe { e.as_ref() };
        let key = e.r#type() == NSEventType::KeyDown;
        let recent = ACTIVE.lock().unwrap().is_some_and(|t| t.elapsed() < SESSION_IDLE);
        if key && e.keyCode() == 53 && recent {
            *STOPPED.lock().unwrap() = Some(Instant::now());
            crate::cursor::hide();
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
    // An app that believes it is active can take the front by itself (Chrome on super+l). The
    // moment it does without the user's input, hand the front straight back, before the
    // window server shows it.
    let activated = RcBlock::new(|n: std::ptr::NonNull<NSNotification>| {
        let app = unsafe { n.as_ref() }.userInfo().and_then(|u| u.objectForKey(unsafe { NSWorkspaceApplicationKey }));
        let pid = app.and_then(|a| a.downcast::<NSRunningApplication>().ok()).map(|a| a.processIdentifier());
        let user = TOUCHED.lock().unwrap().iter().any(|(_, t)| t.elapsed() < Duration::from_secs(1));
        if let (Some(pid), Some((target, front)), false) = (pid, *GUARD.lock().unwrap(), user) {
            if pid == target {
                crate::input::set_front(front);
            }
        }
    });
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    std::mem::forget(unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(NSWorkspaceDidActivateApplicationNotification), None, None, &activated)
    });
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

/// Keep `target` from taking the front from `front` while the agent works in it.
pub fn guard(target: i32, front: Option<i32>) {
    if let Some(front) = front.filter(|&f| f != target) {
        *GUARD.lock().unwrap() = Some((target, front));
    }
}

/// When the user last pressed a key, clicked or scrolled in the app.
pub fn touched(pid: i32) -> Option<Instant> {
    TOUCHED.lock().unwrap().iter().find(|(p, _)| *p == pid).map(|(_, t)| *t)
}
