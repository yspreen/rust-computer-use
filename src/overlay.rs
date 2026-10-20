//! What the user sees while the agent works: a pointer drawn where it acts (the real cursor
//! never moves), and Esc to stop it, like the original's overlay.
use crate::input::Pt;
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::MainThreadMarker;
use objc2_app_kit::*;
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Last agent action, and when the user pressed Esc during a session.
static ACTIVE: Mutex<Option<Instant>> = Mutex::new(None);
static STOPPED: Mutex<Option<Instant>> = Mutex::new(None);
const SESSION_IDLE: Duration = Duration::from_secs(30);

thread_local! {
    static POINTER: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
}

/// Run AppKit on the main thread (never returns); `work` runs the MCP loop on another thread.
pub fn run(work: impl FnOnce() + Send + 'static) -> ! {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory); // no Dock icon, never frontmost
    let esc = RcBlock::new(|e: std::ptr::NonNull<NSEvent>| {
        let recent = ACTIVE.lock().unwrap().is_some_and(|t| t.elapsed() < SESSION_IDLE);
        if unsafe { e.as_ref() }.keyCode() == 53 && recent {
            *STOPPED.lock().unwrap() = Some(Instant::now());
            hide();
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

/// Draw the agent's pointer at `p` (global top-left points), after the event is posted, and
/// hide it after a pause.
pub fn point_at(p: Pt) {
    DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().unwrap();
        POINTER.with(|w| {
            let mut w = w.borrow_mut();
            let win = w.get_or_insert_with(|| pointer_window(mtm));
            let screen = NSScreen::screens(mtm).firstObject().map(|s| s.frame().size.height).unwrap_or(0.0);
            // One point right of and below the target, so the pointer's window never sits under
            // the spot being clicked (the window server would report it as the hit window).
            win.setFrameOrigin(NSPoint::new(p.x + 1.0, screen - p.y - 33.0));
            win.orderFrontRegardless();
        });
    });
    let shown = Instant::now();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        if ACTIVE.lock().unwrap().is_none_or(|t| t <= shown) {
            hide();
        }
    });
}

fn hide() {
    DispatchQueue::main().exec_async(|| {
        POINTER.with(|w| {
            if let Some(w) = w.borrow().as_ref() {
                w.orderOut(None);
            }
        })
    });
}

/// A click-through, shadowless window holding the system arrow, above everything.
fn pointer_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(32.0, 32.0));
    let win = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(), frame, NSWindowStyleMask::Borderless, NSBackingStoreType::Buffered, false,
        )
    };
    unsafe { win.setReleasedWhenClosed(false) };
    win.setOpaque(false);
    win.setBackgroundColor(Some(&NSColor::clearColor()));
    win.setHasShadow(false);
    win.setIgnoresMouseEvents(true);
    win.setLevel(1000); // above the menu bar and full-screen apps
    win.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    let view = NSImageView::imageViewWithImage(&NSCursor::arrowCursor().image(), mtm);
    view.setFrame(frame);
    win.setContentView(Some(&view));
    win
}
