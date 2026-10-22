//! The original's SyntheticAppFocusEnforcer.safelyEnforceActiveState: let a background app
//! believe it is active while the user's app stays in front, with the screen frozen so the
//! activation never shows.
use crate::ax::{self, Cf};
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Ev = *const c_void;
pub type TapCallback = extern "C" fn(*const c_void, u32, Ev, *mut c_void) -> Ev;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(loc: u32, place: u32, options: u32, mask: u64, cb: TapCallback, ctx: *mut c_void) -> *const c_void;
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
    symbol(name).map(|f| unsafe { std::mem::transmute::<usize, extern "C" fn(i32) -> i32>(f) })
}

pub(crate) fn symbol(name: &std::ffi::CStr) -> Option<usize> {
    let f = unsafe { dlsym(-2isize as _, name.as_ptr()) };
    (!f.is_null()).then_some(f as usize)
}

/// What the tap on the user's app saw: any (de)activation, an activation, and when.
struct Seen {
    any: bool,
    activated: bool,
    at: Instant,
}

/// An active event tap running on its own thread, as the original's EventTap does. `pid` taps
/// one process (CGEventTapCreateForPid); None taps the annotated session.
pub struct Tap {
    port: usize,
    run_loop: usize,
    stopped: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// What a tap's trampoline needs: the real callback, its context, and the tap to re-enable.
struct Hook {
    cb: TapCallback,
    ctx: usize,
    port: std::sync::atomic::AtomicUsize,
}

/// Re-enable a tap the system disabled (timeout or user input), as the original does; pass
/// everything else on.
extern "C" fn trampoline(proxy: *const c_void, ty: u32, e: Ev, hook: *mut c_void) -> Ev {
    let hook = unsafe { &*(hook as *const Hook) };
    if ty == 0xFFFF_FFFE || ty == 0xFFFF_FFFF {
        unsafe { CGEventTapEnable(hook.port.load(std::sync::atomic::Ordering::Relaxed) as _, true) };
        return e;
    }
    (hook.cb)(proxy, ty, e, hook.ctx as _)
}

impl Tap {
    pub fn start<T: Send + Sync + 'static>(pid: Option<i32>, place: u32, types: &[u32], cb: TapCallback, ctx: Arc<T>) -> Option<Tap> {
        let mask = types.iter().fold(0u64, |m, t| m | 1 << t);
        let stopped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = stopped.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || unsafe {
            let hook = Box::new(Hook { cb, ctx: Arc::as_ptr(&ctx) as usize, port: Default::default() });
            let hook_ptr = &*hook as *const Hook as *mut c_void;
            let port = match pid {
                Some(pid) => CGEventTapCreateForPid(pid, place, 0, mask, trampoline, hook_ptr),
                None => CGEventTapCreate(2, place, 0, mask, trampoline, hook_ptr), // kCGAnnotatedSessionEventTap
            };
            if port.is_null() {
                let _ = tx.send(None);
                return;
            }
            hook.port.store(port as usize, std::sync::atomic::Ordering::Relaxed);
            let src = Cf(CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0));
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, src.0, kCFRunLoopCommonModes);
            CGEventTapEnable(port, true);
            let _ = tx.send(Some((port as usize, CFRetain(rl) as usize)));
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                core_foundation_sys::runloop::CFRunLoopRunInMode(core_foundation_sys::runloop::kCFRunLoopDefaultMode, 0.1, false as _);
            }
            CFMachPortInvalidate(port);
            drop(Cf(port));
            drop(hook);
            drop(ctx);
        });
        match rx.recv().ok().flatten() {
            Some((port, run_loop)) => Some(Tap { port, run_loop, stopped, thread: Some(thread) }),
            None => { let _ = thread.join(); None }
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            self.stopped.store(true, std::sync::atomic::Ordering::Release);
            CGEventTapEnable(self.port as _, false);
            CFRunLoopStop(self.run_loop as _);
            if let Some(thread) = self.thread.take() { let _ = thread.join(); }
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
extern "C" fn target_tap(_: *const c_void, _ty: u32, e: Ev, ctx: *mut c_void) -> Ev {
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
        Tap::start(Some(user), 0, &[13, 20, 19], user_tap, seen.clone()),
        Tap::start(Some(target), 0, &[10, 11, 12], target_tap, redirect.clone()),
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

#[derive(Default)]
pub struct State {
    believes_active: bool,
    pub believes_focus: bool,
    active: bool,
}

struct EnforcerContext {
    pid: i32,
    state: Arc<Mutex<State>>,
}

extern "C" fn activation(_: Ev, ty: u32, e: Ev, ctx: *mut c_void) -> Ev {
    let ctx = unsafe { &*(ctx as *const EnforcerContext) };
    let mut s = ctx.state.lock().unwrap();
    let sub = unsafe { CGEventGetIntegerValueField(e, 83) };
    match (ty, sub) {
        (20, _) | (13, 1) => { s.believes_active = true; s.believes_focus = true; }
        (19, _) | (13, 2) if s.believes_active && !s.active => return std::ptr::null(),
        (13, 22 | 23) if s.believes_active && !s.active => return std::ptr::null(),
        (19, _) | (13, 2) => { s.believes_active = false; s.believes_focus = false; }
        _ => {}
    }
    e
}

extern "C" fn user_click(_: Ev, _: u32, e: Ev, ctx: *mut c_void) -> Ev {
    let ctx = unsafe { &*(ctx as *const EnforcerContext) };
    let s = ctx.state.lock().unwrap();
    if !(s.believes_active && !s.active) { return e; }
    drop(s);
    let field = |f| unsafe { CGEventGetIntegerValueField(e, f) };
    if field(41) != std::process::id() as i64 && field(40) != ctx.pid as i64
        && crate::window_owner(field(51) as u32) == Some(ctx.pid) {
        if let Some(app) = objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(ctx.pid) {
            #[allow(deprecated)]
            app.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions(0));
        }
    }
    e
}

pub struct Enforcer {
    pid: i32,
    state: Arc<Mutex<State>>,
    taps: Vec<Tap>,
    preventer: Arc<crate::preventer::Preventer>,
    observer: objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>>,
}

impl Enforcer {
    pub fn new(pid: i32) -> Result<Self, String> {
        let active = crate::apps::frontmost() == Some(pid);
        let state = Arc::new(Mutex::new(State { believes_active: active, believes_focus: false, active }));
        let preventer = crate::preventer::Preventer::register(pid, &state)?;
        state.lock().unwrap().believes_focus = preventer.focused(pid);
        let ctx = Arc::new(EnforcerContext { pid, state: state.clone() });
        let taps = (|| Ok::<_, String>(vec![
            Tap::start(Some(pid), 0, &[13, 19, 20], activation, ctx.clone()).ok_or("Could not observe app activation")?,
            Tap::start(None, 0, &[1, 2, 3, 4, 6, 7, 25, 26, 27], user_click, ctx).ok_or("Could not protect background clicks")?,
            preventer.keyboard_tap(pid).ok_or("Could not protect keyboard focus")?,
        ]))();
        let taps = match taps { Ok(t) => t, Err(e) => { preventer.unregister(&state); return Err(e); } };
        let observed = state.clone();
        let block = block2::RcBlock::new(move |_: std::ptr::NonNull<objc2_foundation::NSNotification>| {
            observed.lock().unwrap().active = crate::apps::frontmost() == Some(pid);
        });
        let observer = unsafe { objc2_app_kit::NSWorkspace::sharedWorkspace().notificationCenter()
            .addObserverForName_object_queue_usingBlock(Some(objc2_app_kit::NSWorkspaceDidActivateApplicationNotification), None, None, &block) };
        Ok(Self { pid, state, taps, preventer, observer })
    }

    pub fn believes_focus(&self) -> bool { self.state.lock().unwrap().believes_focus }

    pub fn enforce(&self, point: Option<crate::input::Pt>) -> Result<(), String> {
        if self.believes_focus() { return Ok(()); }
        safely_enforce(self.pid, || {
            crate::input::believe_active(self.pid, point);
            let mut s = self.state.lock().unwrap();
            s.believes_active = true;
            s.believes_focus = true;
        })
    }
}

impl Drop for Enforcer {
    fn drop(&mut self) {
        self.taps.clear();
        let s = self.state.lock().unwrap();
        if s.believes_active && !s.active { crate::input::post_app_event(self.pid, 2, 0); }
        drop(s);
        unsafe { objc2_app_kit::NSWorkspace::sharedWorkspace().notificationCenter().removeObserver((*self.observer).as_ref()) };
        self.preventer.unregister(&self.state);
    }
}
