//! The original's SyntheticAppFocusEnforcer.safelyEnforceActiveState: let a background app
//! believe it is active while the user's app stays in front, with the screen frozen so the
//! activation never shows.
use crate::ax::{self, Cf};
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Ev = *const c_void;
type TapCallback = extern "C" fn(*const c_void, u32, Ev, *mut c_void) -> Ev;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreateForPid(pid: i32, place: u32, options: u32, mask: u64, cb: TapCallback, ctx: *mut c_void) -> *const c_void;
    fn CGEventTapEnable(tap: *const c_void, on: bool);
    fn CGEventGetIntegerValueField(e: Ev, field: u32) -> i64;
    fn CGEventPostToPid(pid: i32, e: Ev);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(a: *const c_void, port: *const c_void, order: isize) -> *const c_void;
    fn CFMachPortInvalidate(port: *const c_void);
    fn CFRunLoopGetCurrent() -> *const c_void;
    fn CFRunLoopAddSource(rl: *const c_void, src: *const c_void, mode: *const c_void);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: *const c_void);
    fn CFRetain(v: *const c_void) -> *const c_void;
    static kCFRunLoopCommonModes: *const c_void;
}

extern "C" {
    fn dlsym(handle: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
}

/// A WindowServer SPI function, resolved at run time as the original does (CoreGraphics
/// re-exports SkyLight's SLS functions under their CGS names).
fn spi(name: &std::ffi::CStr) -> Option<extern "C" fn(i32) -> i32> {
    let f = unsafe { dlsym(-2isize as _, name.as_ptr()) }; // RTLD_DEFAULT
    (!f.is_null()).then(|| unsafe { std::mem::transmute::<*mut c_void, extern "C" fn(i32) -> i32>(f) })
}

/// What the tap on the user's app saw: any (de)activation, an activation, and when.
struct Seen {
    any: bool,
    activated: bool,
    at: Instant,
}

/// A CGEventTapCreateForPid tap (head insert, active) running on its own thread, as the
/// original's EventTap does.
struct Tap {
    port: usize,
    run_loop: usize,
}

impl Tap {
    fn start(pid: i32, types: &[u32], cb: TapCallback, ctx: *mut c_void) -> Option<Tap> {
        let mask = types.iter().fold(0u64, |m, t| m | 1 << t);
        let ctx = ctx as usize;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || unsafe {
            let port = CGEventTapCreateForPid(pid, 0, 0, mask, cb, ctx as _);
            if port.is_null() {
                return tx.send(None).unwrap();
            }
            let src = Cf(CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0));
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, src.0, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            tx.send(Some(Tap { port: port as usize, run_loop: CFRetain(rl) as usize })).unwrap();
            CFRunLoopRun();
            drop(Cf(port));
        });
        rx.recv().ok().flatten()
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            CGEventTapEnable(self.port as _, false);
            CFMachPortInvalidate(self.port as _);
            CFRunLoopStop(self.run_loop as _);
            drop(Cf(self.run_loop as _));
        }
    }
}

struct UserTap {
    seen: Mutex<Seen>,
}

/// On the user's app: swallow the activation changes the target's activation causes, so the
/// user's app never learns it lost the front. Types 13 (AppKit-defined, subtype 1 activated,
/// 2 deactivated, 22/23 window resigned key/main appearance), 20 and 19.
extern "C" fn user_tap(_: *const c_void, ty: u32, e: Ev, ctx: *mut c_void) -> Ev {
    if ty == 0xFFFF_FFFE || ty == 0xFFFF_FFFF {
        return e;
    }
    let state = unsafe { &*(ctx as *const UserTap) };
    let mut seen = state.seen.lock().unwrap();
    seen.at = Instant::now();
    let sub = unsafe { CGEventGetIntegerValueField(e, 83) };
    let sub = i16::try_from(sub).ok();
    match (ty, sub) {
        (20, _) | (13, Some(1)) => (seen.any, seen.activated) = (true, true),
        (19, _) | (13, Some(2)) => seen.any = true,
        (13, Some(22 | 23)) => {}
        _ => return e,
    }
    std::ptr::null()
}

struct TargetTap {
    own: i32,
    target: i32,
    user: i32,
}

/// On the target: keys the user types while the target is briefly active go to the user's app.
extern "C" fn target_tap(_: *const c_void, ty: u32, e: Ev, ctx: *mut c_void) -> Ev {
    if ty == 0xFFFF_FFFE || ty == 0xFFFF_FFFF {
        return e;
    }
    let t = unsafe { &*(ctx as *const TargetTap) };
    let field = |f| unsafe { CGEventGetIntegerValueField(e, f) } as i32;
    if field(41) != t.own && field(40) == t.target {
        unsafe { CGEventPostToPid(t.user, e) };
        return std::ptr::null();
    }
    e
}

/// Frontmost app as (pid, NSRunningApplication).
fn front() -> Option<(i32, objc2::rc::Retained<objc2_app_kit::NSRunningApplication>)> {
    objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| (a.processIdentifier(), a))
}

/// Does a window of `target` lie above the user's window in the same layer?
fn covered(user_window: u32, target: i32) -> bool {
    let list = crate::window_list(); // front to back, on screen, no desktop
    let Some(i) = list.iter().position(|w| w.0 == user_window) else { return false };
    list[..i].iter().any(|w| w.1 == target && w.2 == list[i].2)
}

/// The original's safelyEnforceActiveState(for:). `enforce` is its enforceActiveState.
pub fn safely_enforce(target: i32, enforce: impl FnOnce()) -> Result<(), String> {
    let Some((user, user_app)) = front().filter(|(p, _)| *p != target) else {
        enforce();
        wait_frontmost(target, 0.25);
        return Ok(());
    };
    let user_el = ax::app_element(user);
    let user_win = ax::attr(user_el.0, "AXFocusedWindow");
    let user_win_id = user_win.as_ref().and_then(|w| ax::window_id(w.0));

    let seen = Arc::new(UserTap { seen: Mutex::new(Seen { any: false, activated: false, at: Instant::now() }) });
    let redirect = Arc::new(TargetTap { own: std::process::id() as i32, target, user });
    let taps = [
        Tap::start(user, &[13, 20, 19], user_tap, Arc::as_ptr(&seen) as _),
        Tap::start(target, &[10, 11, 12], target_tap, Arc::as_ptr(&redirect) as _),
    ];

    let cid = spi(c"CGSMainConnectionID").map_or(0, |f| f(0));
    let (disable, reenable) = (spi(c"CGSDisableUpdate"), spi(c"CGSReenableUpdate"));
    disable.map(|f| f(cid));
    enforce();
    seen.seen.lock().unwrap().at = Instant::now();
    let start = Instant::now();
    let mut reactivated_at: Option<Instant> = None;
    let result = loop {
        let elapsed = start.elapsed();
        match front().map(|f| f.0) {
            Some(p) if p == target => {
                if reactivated_at.is_none_or(|t| t.elapsed() >= Duration::from_millis(50)) {
                    {
                        let mut s = seen.seen.lock().unwrap();
                        s.activated = false;
                        s.at = Instant::now();
                    }
                    #[allow(deprecated)]
                    if !user_app.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions(0)) {
                        break Err("Could not reactivate the frontmost app".to_string());
                    }
                    reactivated_at = Some(Instant::now());
                }
            }
            Some(p) if p == user => {
                if let (Some(w), Some(id)) = (&user_win, user_win_id) {
                    if elapsed < Duration::from_secs(1) && covered(id, target) {
                        let _ = ax::perform(w.0, "AXRaise");
                        seen.seen.lock().unwrap().at = Instant::now();
                    }
                }
                let s = seen.seen.lock().unwrap();
                let quiet = s.at.elapsed() >= Duration::from_millis(100);
                if reactivated_at.is_some() && !s.activated {
                    // Wait for the user's app to see itself activated again.
                } else if !(reactivated_at.is_some() || s.any) || quiet {
                    break Ok(());
                }
            }
            _ => break Err("The frontmost app changed".to_string()),
        }
        if elapsed >= Duration::from_secs(2) {
            break Err("Timed out keeping the frontmost app in front".to_string());
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    reenable.map(|f| f(cid));
    drop(taps);
    result
}

/// ApplicationUIElement.waitUntilAppBelievesItIsFrontmost(timeout:): poll AXFrontmost.
fn wait_frontmost(pid: i32, timeout: f64) {
    let app = ax::app_element(pid);
    let start = Instant::now();
    while start.elapsed().as_secs_f64() < timeout {
        if ax::attr(app.0, "AXFrontmost").is_some_and(|v| ax::to_bool(v.0)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Whether the target believes it has focus, as the enforcer's state tracks it. Set by an
/// enforcement; cleared when the user's front app changes since then.
static BELIEVES: Mutex<Option<(i32, Option<i32>)>> = Mutex::new(None);

pub fn believes_focus(pid: i32) -> bool {
    *BELIEVES.lock().unwrap() == Some((pid, crate::apps::frontmost()))
}

pub fn set_believes_focus(pid: i32) {
    *BELIEVES.lock().unwrap() = Some((pid, crate::apps::frontmost()));
}
