//! SystemFocusStealPreventer, recovered from SkyComputerUseService 1001501.
//! Process notifications describe key-focus ownership independently of frontmost activation.
use crate::{ax, focus::{Ev, State, Tap}};
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use std::{ffi::c_void, sync::{Arc, Mutex, Weak}};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn GetProcessPID(psn: *const u64, pid: *mut i32) -> i32;
    fn AXUIElementGetPid(e: Ev, pid: *mut i32) -> i32;
}
#[link(name = "proc")]
extern "C" {
    fn proc_listpids(kind: u32, info: u32, buf: *mut c_void, size: i32) -> i32;
    fn proc_name(pid: i32, buf: *mut c_void, size: u32) -> i32;
}
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventGetIntegerValueField(e: Ev, field: u32) -> i64;
    fn CGEventPostToPid(pid: i32, e: Ev);
}

pub fn key_focus() -> Option<i32> {
    let get = crate::focus::symbol(c"CPSGetKeyFocusProcess")?;
    let get: extern "C" fn(*mut u64, *mut u8) -> i32 = unsafe { std::mem::transmute(get) };
    let (mut psn, mut flag, mut pid) = (0, 0, 0);
    (get(&mut psn, &mut flag) == 0 && unsafe { GetProcessPID(&psn, &mut pid) } == 0).then_some(pid)
}

fn view_bridge_pid() -> Option<i32> {
    let size = unsafe { proc_listpids(1, 0, std::ptr::null_mut(), 0) };
    let mut pids = vec![0i32; (size.max(0) as usize / 4) + 128];
    let bytes = unsafe { proc_listpids(1, 0, pids.as_mut_ptr() as _, (pids.len() * 4) as i32) };
    pids.into_iter().take(bytes.max(0) as usize / 4).find(|p| {
        let mut name = [0u8; 256];
        let n = unsafe { proc_name(*p, name.as_mut_ptr() as _, name.len() as _) };
        n > 0 && &name[..n as usize] == b"ViewBridgeAuxiliary"
    })
}

#[derive(Clone, Copy)]
struct Owner {
    pid: Option<i32>,
    view_bridge: bool,
}

fn classify(pid: i32, vb: Option<i32>) -> Owner {
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid);
    let view_bridge = vb == Some(pid);
    if !view_bridge && !app.as_ref().is_some_and(|a| a.activationPolicy() == NSApplicationActivationPolicy::Prohibited) {
        return Owner { pid: Some(pid), view_bridge: false };
    }
    let root = ax::app_element(pid);
    for name in ["AXFocusedUIElement", "AXFocusedWindow", "AXMainWindow"] {
        if let Some(e) = ax::attr(root.0, name) {
            let mut host = 0;
            if unsafe { AXUIElementGetPid(e.0, &mut host) } == 0 {
                if NSRunningApplication::runningApplicationWithProcessIdentifier(host)
                    .is_some_and(|a| a.activationPolicy() != NSApplicationActivationPolicy::Prohibited) {
                    return Owner { pid: Some(host), view_bridge };
                }
            }
        }
    }
    let host = app.and_then(|a| a.localizedName()).and_then(|name| {
        NSWorkspace::sharedWorkspace().runningApplications().iter()
            .find(|a| a.activationPolicy() != NSApplicationActivationPolicy::Prohibited && a.localizedName().as_deref() == Some(&name))
            .map(|a| a.processIdentifier())
    }).or_else(crate::apps::frontmost);
    Owner { pid: host, view_bridge }
}

struct Pending {
    thief: i32,
    app: Option<i32>,
    victim: i32,
    released: bool,
    returned: bool,
}

struct Focus {
    current: Owner,
    previous: Option<Owner>,
    pending: Option<Pending>,
    view_bridge_suppressed: bool,
}

struct Shared {
    vb: Option<i32>,
    focus: Mutex<Focus>,
    registrations: Mutex<Vec<(i32, Weak<Mutex<State>>)>>,
}

pub struct Preventer {
    _taps: Vec<Tap>,
    shared: Arc<Shared>,
}

static SHARED: Mutex<Weak<Preventer>> = Mutex::new(Weak::new());

impl Preventer {
    pub fn register(pid: i32, state: &Arc<Mutex<State>>) -> Result<Arc<Self>, String> {
        let mut singleton = SHARED.lock().unwrap();
        let preventer = match singleton.upgrade() {
            Some(p) => p,
            None => {
                let vb = view_bridge_pid();
                let owner = key_focus().map(|p| classify(p, vb))
                    .unwrap_or(Owner { pid: crate::apps::frontmost(), view_bridge: false });
                let shared = Arc::new(Shared {
                    vb,
                    focus: Mutex::new(Focus { current: owner, previous: None, pending: None, view_bridge_suppressed: false }),
                    registrations: Mutex::new(Vec::new()),
                });
                let mut taps = vec![Tap::start(None, 1, &[21], notifications, shared.clone()).ok_or("Could not watch key-focus notifications")?];
                if let Some(vb) = vb {
                    taps.push(Tap::start(Some(vb), 1, &[10, 11, 12], keyboard, shared.clone()).ok_or("Could not protect ViewBridge keyboard focus")?);
                }
                let p = Arc::new(Self { _taps: taps, shared });
                *singleton = Arc::downgrade(&p);
                p
            }
        };
        preventer.shared.registrations.lock().unwrap().push((pid, Arc::downgrade(state)));
        Ok(preventer)
    }

    pub fn keyboard_tap(&self, pid: i32) -> Option<Tap> {
        Tap::start(Some(pid), 1, &[10, 11, 12], keyboard, self.shared.clone())
    }

    pub fn unregister(&self, state: &Arc<Mutex<State>>) {
        let mut regs = self.shared.registrations.lock().unwrap();
        if let Some(i) = regs.iter().position(|(_, s)| s.ptr_eq(&Arc::downgrade(state))) { regs.remove(i); }
    }

    pub fn focused(&self, pid: i32) -> bool {
        self.shared.focus.lock().unwrap().current.pid == Some(pid)
    }
}

fn field(e: Ev, f: u32) -> i64 { unsafe { CGEventGetIntegerValueField(e, f) } }

extern "C" fn keyboard(_: Ev, _: u32, e: Ev, ctx: *mut c_void) -> Ev {
    if field(e, 41) == std::process::id() as i64 { return e; }
    let shared = unsafe { &*(ctx as *const Shared) };
    let victim = shared.focus.lock().unwrap().pending.as_ref()
        .filter(|p| field(e, 40) == p.thief as i64).map(|p| p.victim);
    if let Some(pid) = victim {
        unsafe { CGEventPostToPid(pid, e) };
        std::ptr::null()
    } else { e }
}

extern "C" fn notifications(_: Ev, _: u32, e: Ev, ctx: *mut c_void) -> Ev {
    let shared = unsafe { &*(ctx as *const Shared) };
    let (target, sub, subject) = (field(e, 40) as i32, field(e, 64) as u16, field(e, 73) as i32);
    let owner = matches!(sub, 0x4000 | 0xf102).then(|| classify(subject, shared.vb));
    let mut state = shared.focus.lock().unwrap();
    match sub {
        0x4000 => {
            let owner = owner.unwrap();
            let thief = shared.registrations.lock().unwrap().iter().any(|r| Some(r.0) == owner.pid);
            state.pending = if thief && owner.pid != state.current.pid {
                Some(Pending { thief: subject, app: owner.pid, victim: target, released: false, returned: false })
            } else { None };
            if state.pending.is_some() { return std::ptr::null(); }
        }
        0x8000 => {
            if let Some(p) = &mut state.pending {
                if p.victim == target && !p.returned { p.returned = true; return std::ptr::null(); }
            }
        }
        0xf102 if shared.vb == Some(target) => {
            let owner = owner.unwrap();
            let prev = state.current;
            state.previous = Some(prev);
            state.current = owner;
            let suppressed = state.pending.as_ref().and_then(|p| p.app);
            let changes: Vec<_> = shared.registrations.lock().unwrap().iter().filter_map(|(pid, s)| {
                if owner.pid == prev.pid || suppressed == Some(*pid) { return None; }
                if Some(*pid) == owner.pid { Some((s.clone(), true)) }
                else if Some(*pid) == prev.pid { Some((s.clone(), false)) } else { None }
            }).collect();
            let mut release = false;
            if let Some(p) = &mut state.pending {
                if p.thief == subject {
                    if !p.released { p.released = true; release = true; }
                } else if p.victim != subject { state.pending = None; }
            }
            let ok = release && crate::focus::symbol(c"CPSReleaseKeyFocusWithID").is_some_and(|f| {
                let f: extern "C" fn(u32) -> i32 = unsafe { std::mem::transmute(f) };
                f(field(e, 71) as u32) == 0
            });
            if owner.view_bridge { state.view_bridge_suppressed = ok; }
            drop(state);
            if !changes.is_empty() {
                std::thread::spawn(move || for (s, focused) in changes {
                    if let Some(s) = s.upgrade() { s.lock().unwrap().believes_focus = focused; }
                });
            }
            return e;
        }
        2 if shared.vb == Some(target) => {
            if state.pending.as_ref().is_some_and(|p| p.thief == subject) { state.pending = None; }
            if state.view_bridge_suppressed { return std::ptr::null(); }
        }
        _ => {}
    }
    e
}
