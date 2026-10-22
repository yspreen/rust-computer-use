//! The focus preventer's menu-dismissal filters, controlled by AX menu notifications.
use crate::{ax, focus::{Ev, Tap}};
use std::{ffi::c_void, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread::JoinHandle};

struct Context {
    pid: i32,
    control_center: bool,
    menu_pid: Mutex<Option<i32>>,
    enabled: AtomicBool,
    taps: Mutex<Vec<Tap>>,
}

extern "C" fn mouse(_: Ev, _: u32, e: Ev, ctx: *mut c_void) -> Ev {
    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" { fn CGEventGetIntegerValueField(e: Ev, field: u32) -> i64; }
    let ctx = unsafe { &*(ctx as *const Context) };
    if ctx.enabled.load(Ordering::Acquire) {
        let window = unsafe { CGEventGetIntegerValueField(e, 51) } as u32;
        if let Some(owner) = crate::window_owner(window) {
            if owner != ctx.pid && Some(owner) != *ctx.menu_pid.lock().unwrap() { return std::ptr::null(); }
        }
    }
    e
}

impl Context {
    fn stop(&self) {
        self.enabled.store(false, Ordering::Release);
        self.taps.lock().unwrap().clear();
    }

    fn start(self: &Arc<Self>, menu_pid: Option<i32>) {
        self.stop();
        *self.menu_pid.lock().unwrap() = menu_pid;
        let types = &[1, 2, 6, 3, 4, 7, 25, 26, 27];
        let mut taps = Vec::new();
        for pid in std::iter::once(self.pid).chain(menu_pid) {
            if let Some(tap) = Tap::start(Some(pid), 0, types, mouse, self.clone()) { taps.push(tap); }
        }
        *self.taps.lock().unwrap() = taps;
        self.enabled.store(true, Ordering::Release);
    }
}

extern "C" fn notification(obs: ax::AXRef, e: ax::AXRef, name: core_foundation_sys::string::CFStringRef, ctx: *mut c_void) {
    let ctx = unsafe { &*(ctx as *const Arc<Context>) };
    match ax::to_string(name as _).as_deref() {
        Some("AXMenuOpened") => {
            let start = std::time::Instant::now();
            while ax::window_id(e).is_none() && start.elapsed().as_secs_f64() < 0.25 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            ctx.start(ax::pid(e));
        }
        Some("AXWindowCreated") if ctx.control_center => {
            ax::observe_element(obs, e, "AXUIElementDestroyed", ctx as *const _ as _);
            ctx.start(ax::pid(e));
        }
        Some("AXMenuClosed" | "AXUIElementDestroyed") => ctx.stop(),
        _ => {}
    }
}

pub struct Watcher {
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    pub fn new(pid: i32) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let thread = std::thread::spawn(move || {
            let control_center = objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
                .and_then(|a| a.bundleIdentifier()).is_some_and(|b| b.to_string() == "com.apple.controlcenter");
            let ctx = Arc::new(Context { pid, control_center, menu_pid: Mutex::new(None), enabled: AtomicBool::new(false), taps: Mutex::new(Vec::new()) });
            let names = &["AXMenuOpened", "AXMenuClosed", "AXWindowCreated"];
            let observer = ax::observe(pid, names, notification, &ctx as *const _ as _);
            if observer.is_none() { return; }
            while !stop.load(Ordering::Acquire) { ax::run_loop_for(0.1); }
            drop(observer);
            ctx.stop();
        });
        Self { stopped, thread: Some(thread) }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}
