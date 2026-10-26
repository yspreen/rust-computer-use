//! Accessibility tree capture and element actions (AXUIElement FFI).
use core_foundation_sys::array::*;
use core_foundation_sys::base::*;
use core_foundation_sys::number::*;
use core_foundation_sys::string::*;
use std::ffi::c_void;

pub type AXRef = *const c_void;
const MAX_NODES: usize = 12000;

/// Display names for standard actions; anything else shows its AX description.
const STANDARD_ACTIONS: &[(&str, &str)] = &[
    ("AXRaise", "Raise"), ("AXPick", "Pick"),
    ("AXIncrement", "Increment"),
    ("AXDecrement", "Decrement"),
    ("AXDelete", "Delete"),
    ("AXCancel", "Cancel"),
    ("AXConfirm", "Confirm"),
    ("AXScrollLeftByPage", "Scroll Left"),
    ("AXScrollRightByPage", "Scroll Right"),
    ("AXScrollUpByPage", "Scroll Up"),
    ("AXScrollDownByPage", "Scroll Down"),
];

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXRef;
    fn AXUIElementGetPid(e: AXRef, pid: *mut i32) -> i32;
    fn AXUIElementCopyAttributeValue(e: AXRef, a: CFStringRef, v: *mut CFTypeRef) -> i32;
    fn AXUIElementSetAttributeValue(e: AXRef, a: CFStringRef, v: CFTypeRef) -> i32;
    fn AXObserverCreate(pid: i32, callback: ObserverCallback, out: *mut AXRef) -> i32;
    fn AXObserverAddNotification(obs: AXRef, e: AXRef, name: CFStringRef, context: *mut c_void) -> i32;
    fn AXObserverGetRunLoopSource(obs: AXRef) -> core_foundation_sys::runloop::CFRunLoopSourceRef;
    fn AXUIElementCopyAttributeNames(e: AXRef, v: *mut CFArrayRef) -> i32;
    fn AXUIElementCopyMultipleAttributeValues(e: AXRef, names: CFArrayRef, opts: u32, v: *mut CFArrayRef) -> i32;
    fn AXValueGetTypeID() -> CFTypeID;
    fn AXUIElementCopyActionNames(e: AXRef, v: *mut CFArrayRef) -> i32;
    fn AXUIElementCopyActionDescription(e: AXRef, a: CFStringRef, v: *mut CFStringRef) -> i32;
    fn AXUIElementIsAttributeSettable(e: AXRef, a: CFStringRef, out: *mut bool) -> i32;
    fn AXUIElementPerformAction(e: AXRef, a: CFStringRef) -> i32;
    fn AXValueGetValue(v: CFTypeRef, t: u32, out: *mut c_void) -> bool;
    fn AXValueCreate(t: u32, v: *const c_void) -> CFTypeRef;
    fn _AXUIElementGetWindow(e: AXRef, id: *mut u32) -> i32;
    fn AXUIElementCopyElementAtPosition(app: AXRef, x: f32, y: f32, out: *mut AXRef) -> i32;
}

/// The app's element at a screen point.
pub fn element_at(pid: i32, x: f64, y: f64) -> Option<Cf> {
    let mut e = std::ptr::null();
    let app = app_element(pid);
    (unsafe { AXUIElementCopyElementAtPosition(app.0, x as f32, y as f32, &mut e) } == 0 && !e.is_null()).then(|| Cf(e))
}

/// Whether the element sits inside a web area.
pub fn in_web_view(e: AXRef) -> bool {
    let mut cur = Some(Cf(unsafe { CFRetain(e) }));
    for _ in 0..300 {
        let Some(c) = cur else { break };
        if attr_str(c.0, "AXRole").as_deref() == Some("AXWebArea") {
            return true;
        }
        cur = attr(c.0, "AXParent");
    }
    false
}

/// Codex's web-click preflight: does the hit element's parent chain contain this element?
pub fn contains_hit(e: AXRef, pid: i32, at: crate::input::Pt) -> bool {
    let mut cur = element_at(pid, at.x, at.y);
    for _ in 0..300 {
        let Some(c) = cur else { break };
        if unsafe { CFEqual(e, c.0) } != 0 { return true; }
        cur = attr(c.0, "AXParent");
    }
    false
}

/// Selection can come from any ancestor, up to the application, not just text roles.
pub fn may_select(e: AXRef) -> bool {
    let mut cur = Some(Cf(unsafe { CFRetain(e) }));
    for _ in 0..300 {
        let Some(c) = cur else { break };
        let role = attr_str(c.0, "AXRole");
        if role.as_deref() == Some("AXApplication") { return false; }
        if !matches!(role.as_deref(), Some("AXMenuItem" | "AXMenuBarItem")) && settable(c.0, "AXSelected") { return true; }
        let mut names = std::ptr::null();
        if unsafe { AXUIElementCopyAttributeNames(c.0, &mut names) } == 0 && !names.is_null() {
            if array(&Cf(names as _)).into_iter().filter_map(to_string)
                .any(|n| matches!(n.as_str(), "AXSelectedRows" | "AXSelectedCells" | "AXSelectedChildren")) { return true; }
        }
        cur = attr(c.0, "AXParent");
    }
    false
}

/// The window server number of a window element.
pub fn window_id(e: AXRef) -> Option<u32> {
    let mut id = 0;
    (unsafe { _AXUIElementGetWindow(e, &mut id) } == 0).then_some(id)
}

pub fn to_bool(v: CFTypeRef) -> bool {
    unsafe { CFGetTypeID(v) == CFBooleanGetTypeID() && CFBooleanGetValue(v as _) }
}

/// Owned CF object, released on drop.
pub struct Cf(pub CFTypeRef);
impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

pub fn cfstr(s: &str) -> Cf {
    unsafe {
        Cf(
            CFStringCreateWithBytes(null(), s.as_ptr(), s.len() as _, kCFStringEncodingUTF8, 0)
                as _,
        )
    }
}
fn null() -> CFAllocatorRef {
    std::ptr::null()
}

pub fn to_string(v: CFTypeRef) -> Option<String> {
    unsafe {
        let t = CFGetTypeID(v);
        if t == CFStringGetTypeID() {
            let len = CFStringGetLength(v as _);
            let mut buf = vec![
                0u8;
                (CFStringGetMaximumSizeForEncoding(len, kCFStringEncodingUTF8) + 1)
                    as usize
            ];
            CFStringGetCString(
                v as _,
                buf.as_mut_ptr() as _,
                buf.len() as _,
                kCFStringEncodingUTF8,
            );
            let s = std::ffi::CStr::from_ptr(buf.as_ptr() as _)
                .to_string_lossy()
                .into_owned();
            (!s.is_empty()).then_some(s)
        } else if t == CFNumberGetTypeID() {
            let mut f = 0f64;
            CFNumberGetValue(v as _, kCFNumberFloat64Type, &mut f as *mut f64 as _);
            if f.fract() == 0.0 {
                return Some(format!("{}", f as i64));
            }
            // 16 significant digits, like %.16g.
            let decimals = (15 - f.abs().log10().floor() as i32).max(0) as usize;
            Some(
                format!("{f:.decimals$}")
                    .trim_end_matches('0')
                    .trim_end_matches('.')
                    .to_string(),
            )
        } else if t == core_foundation_sys::url::CFURLGetTypeID() {
            // Finder hands out file reference URLs (file:///.file/id=…); show the path URL.
            let path = Cf(core_foundation_sys::url::CFURLCreateFilePathURL(null(), v as _, std::ptr::null_mut()) as _);
            let url = if path.0.is_null() { v } else { path.0 };
            to_string(core_foundation_sys::url::CFURLGetString(url as _) as _)
        } else if t == core_foundation_sys::number::CFBooleanGetTypeID() {
            Some(if CFBooleanGetValue(v as _) { "on" } else { "off" }.into())
        } else {
            None
        }
    }
}

pub fn app_element(pid: i32) -> Cf {
    Cf(unsafe { AXUIElementCreateApplication(pid) })
}

/// Apps whose web accessibility this process switched on, and the attribute it used.
static ENABLED: std::sync::Mutex<Vec<(i32, &'static str)>> = std::sync::Mutex::new(Vec::new());

/// Chromium/Electron/Firefox only build their web trees when asked. Like the original, use
/// AXManualAccessibility when the app lists it (Electron), else AXEnhancedUserInterface
/// (Chrome, Firefox). The flag is set on every read: Chrome can drop its web tree while still
/// reporting the flag on, and only a fresh write brings it back. True for apps that take either flag.
pub fn enable_web(pid: i32) -> bool {
    let app = app_element(pid);
    let mut names: CFArrayRef = std::ptr::null();
    if unsafe { AXUIElementCopyAttributeNames(app.0, &mut names) } != 0 || names.is_null() {
        return false;
    }
    let names = Cf(names as _);
    let listed: Vec<String> = array(&names).into_iter().filter_map(to_string).collect();
    let Some(flag) = ["AXManualAccessibility", "AXEnhancedUserInterface"].into_iter().find(|f| listed.iter().any(|n| n == f)) else {
        return false;
    };
    let was_on = attr_str(app.0, flag).as_deref() == Some("on");
    let _ = set_bool(app.0, flag, true); // Chrome applies it but may still report an error
    let mut enabled = ENABLED.lock().unwrap();
    if !was_on && !enabled.contains(&(pid, flag)) {
        enabled.push((pid, flag));
    }
    true
}

/// Switch off what enable_web switched on (the original drops its enablement assertion when
/// the session ends): AXEnhancedUserInterface slows window animations and window managers.
pub fn restore_web() {
    for (pid, flag) in ENABLED.lock().unwrap().drain(..) {
        let _ = set_bool(app_element(pid).0, flag, false);
    }
}

pub type ObserverCallback = extern "C" fn(AXRef, AXRef, CFStringRef, *mut c_void);

pub fn pid(e: AXRef) -> Option<i32> {
    let mut pid = 0;
    (unsafe { AXUIElementGetPid(e, &mut pid) } == 0).then_some(pid)
}

pub fn observe_element(obs: AXRef, e: AXRef, name: &str, context: *mut c_void) {
    unsafe { AXObserverAddNotification(obs, e, cfstr(name).0 as _, context) };
}

/// An AXObserver for some of an app's notifications, scheduled on this thread's run loop
/// (run it with run_loop_for). Dropping it unschedules it.
pub fn observe(pid: i32, names: &[&str], callback: ObserverCallback, context: *mut c_void) -> Option<Cf> {
    let mut obs: AXRef = std::ptr::null();
    if unsafe { AXObserverCreate(pid, callback, &mut obs) } != 0 || obs.is_null() {
        return None;
    }
    let app = app_element(pid);
    for n in names {
        unsafe { AXObserverAddNotification(obs, app.0, cfstr(n).0 as _, context) };
    }
    unsafe { core_foundation_sys::runloop::CFRunLoopAddSource(core_foundation_sys::runloop::CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(obs), core_foundation_sys::runloop::kCFRunLoopDefaultMode) };
    Some(Cf(obs))
}

pub fn run_loop_for(seconds: f64) {
    unsafe { core_foundation_sys::runloop::CFRunLoopRunInMode(core_foundation_sys::runloop::kCFRunLoopDefaultMode, seconds, 0) };
}

pub fn attr(e: AXRef, name: &str) -> Option<Cf> {
    let mut v: CFTypeRef = std::ptr::null();
    let n = cfstr(name);
    (unsafe { AXUIElementCopyAttributeValue(e, n.0 as _, &mut v) } == 0 && !v.is_null())
        .then(|| Cf(v))
}
pub fn attr_str(e: AXRef, name: &str) -> Option<String> {
    attr(e, name).and_then(|v| to_string(v.0))
}
/// Web URLs display without their scheme or "www." ("example.com/"); others stay whole.
pub fn short_url(url: &str) -> &str {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    rest.map_or(url, |r| r.strip_prefix("www.").unwrap_or(r))
}
pub fn settable(e: AXRef, name: &str) -> bool {
    let mut ok = false;
    let n = cfstr(name);
    unsafe { AXUIElementIsAttributeSettable(e, n.0 as _, &mut ok) == 0 && ok }
}
pub fn set_bool(e: AXRef, name: &str, on: bool) -> Result<(), String> {
    set_attr(e, name, unsafe { if on { kCFBooleanTrue } else { kCFBooleanFalse } } as _)
}

pub fn set_attr(e: AXRef, name: &str, v: CFTypeRef) -> Result<(), String> {
    let n = cfstr(name);
    match unsafe { AXUIElementSetAttributeValue(e, n.0 as _, v) } {
        0 => Ok(()),
        err => Err(format!("setting {name} failed (AXError {err})")),
    }
}

pub fn elements(v: &Cf) -> Vec<CFTypeRef> {
    array(v)
}

fn array(v: &Cf) -> Vec<CFTypeRef> {
    unsafe {
        if CFGetTypeID(v.0) != CFArrayGetTypeID() {
            return vec![];
        }
        (0..CFArrayGetCount(v.0 as _))
            .map(|i| CFArrayGetValueAtIndex(v.0 as _, i))
            .collect()
    }
}

/// (AX name, human description) for every action of the element.
pub fn actions(e: AXRef) -> Vec<(String, String)> {
    let mut names: CFArrayRef = std::ptr::null();
    if unsafe { AXUIElementCopyActionNames(e, &mut names) } != 0 || names.is_null() {
        return vec![];
    }
    let names = Cf(names as _);
    array(&names)
        .into_iter()
        .filter_map(|n| {
            let name = to_string(n)?;
            let mut d: CFStringRef = std::ptr::null();
            unsafe { AXUIElementCopyActionDescription(e, n as _, &mut d) };
            let desc = (!d.is_null())
                .then(|| Cf(d as _))
                .and_then(|d| to_string(d.0));
            let standard = STANDARD_ACTIONS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, l)| l.to_string());
            let label = standard
                .or(desc)
                .unwrap_or_else(|| name.trim_start_matches("AX").to_string());
            Some((name, label))
        })
        .collect()
}

pub fn perform(e: AXRef, action: &str) -> Result<(), String> {
    let n = cfstr(action);
    match unsafe { AXUIElementPerformAction(e, n.0 as _) } {
        0 => Ok(()),
        err => Err(format!("action {action} failed (AXError {err})")),
    }
}

/// Element frame in global screen points: (x, y, w, h).
/// UIElementProtocol.clickablePoint(): the frame's midpoint, or None when the ancestors that
/// clip it (scroll areas, groups with a scroll action, and the enclosing window, sheet, menu or
/// popover) leave that point hidden.
pub fn clickable_point(e: AXRef) -> Option<(f64, f64)> {
    if let Some(p) = chess_point(e) { return Some(p); }
    let (x, y, w, h) = frame(e)?;
    let mid = (x + w / 2.0, y + h / 2.0);
    let mut vis = (f64::NEG_INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::INFINITY); // x0, y0, x1, y1
    let mut cur = Some(Cf(unsafe { CFRetain(e) }));
    while let Some(c) = cur {
        let role = attr_str(c.0, "AXRole");
        let top = matches!(role.as_deref(), Some("AXWindow" | "AXSheet" | "AXMenu" | "AXPopover"));
        match role.as_deref() {
            Some("AXApplication") => break,
            Some("AXScrollArea") => {}
            Some("AXGroup") if actions(c.0).iter().any(|(n, _)| n.starts_with("AXScroll")) => {}
            _ if top => {}
            _ => { cur = attr(c.0, "AXParent"); continue; }
        }
        let (fx, fy, fw, fh) = frame(c.0)?;
        vis = (vis.0.max(fx), vis.1.max(fy), vis.2.min(fx + fw), vis.3.min(fy + fh));
        if vis.0 >= vis.2 || vis.1 >= vis.3 { return None; }
        if top { break; }
        cur = attr(c.0, "AXParent");
    }
    (mid.0 >= vis.0 && mid.0 < vis.2 && mid.1 >= vis.1 && mid.1 < vis.3).then_some(mid)
}

type Rect = (f64, f64, f64, f64);

fn children(e: AXRef) -> Vec<Cf> {
    attr(e, "AXChildren").map(|v| array(&v).iter().map(|&c| Cf(unsafe { CFRetain(c) })).collect()).unwrap_or_default()
}

fn number(v: CFTypeRef) -> Option<f64> {
    let mut f = 0f64;
    (unsafe { CFGetTypeID(v) == CFNumberGetTypeID() && CFNumberGetValue(v as _, kCFNumberFloat64Type, &mut f as *mut f64 as _) }).then_some(f)
}

/// UIElementProtocol.scrollToVisible(frame:), run before every element click: walking up from the
/// element to its window, sheet, menu or popover, perform AXScrollToVisible where offered, set it
/// on cells that take it, and scroll each scroll area so the frame shows. If anything scrolled,
/// wait up to 0.5 s (polling every 25 ms) for the frame center to become visible.
pub fn scroll_to_visible(e: AXRef) {
    let Some(target) = frame(e) else { return };
    let mut scrolled = false;
    let mut cur = Some(Cf(unsafe { CFRetain(e) }));
    while let Some(c) = cur {
        let role = attr_str(c.0, "AXRole");
        if matches!(role.as_deref(), Some("AXWindow" | "AXSheet" | "AXMenu" | "AXPopover")) { break; }
        if actions(c.0).iter().any(|(n, _)| n == "AXScrollToVisible") {
            scrolled |= perform(c.0, "AXScrollToVisible").is_ok();
        }
        if role.as_deref() == Some("AXCell") && settable(c.0, "AXScrollToVisible") {
            scrolled |= set_bool(c.0, "AXScrollToVisible", true).is_ok();
        }
        if role.as_deref() == Some("AXScrollArea") {
            scrolled |= scroll_area(c.0, target);
        }
        cur = attr(c.0, "AXParent");
    }
    if scrolled {
        let start = std::time::Instant::now();
        while clickable_point(e).is_none() && start.elapsed().as_secs_f64() < 0.5 {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

/// Scroll one scroll area so `t` shows: per axis where `t` sticks out, move that axis's scroll bar
/// (centering `t` when it fits, else aligning the nearer edge), with the scrollable length taken
/// from the union of the non-scroll-bar children. Without a usable scroll bar, fall back to
/// AXMakeScreenRectVisible.
fn scroll_area(area: AXRef, t: Rect) -> bool {
    if !(t.2 > 0.0 && t.3 > 0.0 && t.2.is_finite() && t.3.is_finite()) { return false; }
    let Some(a) = frame(area) else { return false };
    if t.0 >= a.0 && t.1 >= a.1 && t.0 + t.2 <= a.0 + a.2 && t.1 + t.3 <= a.1 + a.3 { return false; }
    let mut content: Option<Rect> = None;
    for child in children(area) {
        if attr_str(child.0, "AXRole").as_deref() == Some("AXScrollBar") { continue; }
        if let Some(f) = frame(child.0) {
            content = Some(match content {
                None => f,
                Some(c) => { let (x0, y0) = (c.0.min(f.0), c.1.min(f.1)); (x0, y0, (c.0 + c.2).max(f.0 + f.2) - x0, (c.1 + c.3).max(f.1 + f.3) - y0) }
            });
        }
    }
    let mut moved = false;
    for (bar_attr, (t0, tl), (a0, al), (c0, cl)) in [
        ("AXHorizontalScrollBar", (t.0, t.2), (a.0, a.2), content.map_or((0.0, 0.0), |c| (c.0, c.2))),
        ("AXVerticalScrollBar", (t.1, t.3), (a.1, a.3), content.map_or((0.0, 0.0), |c| (c.1, c.3))),
    ] {
        let (t1, a1) = (t0 + tl, a0 + al);
        if !(t0 < a0 || t1 > a1) { continue; }
        let Some(bar) = attr(area, bar_attr) else { continue };
        if attr(bar.0, "AXEnabled").is_some_and(|v| !to_bool(v.0)) { continue; }
        let range = cl - al;
        if range <= 0.0 { continue; }
        let delta = if tl <= al { (t0 + t1 - a0 - a1) * 0.5 } else if a0 <= t0 { if t1 <= a1 { continue } t1 - a1 } else { t0 - a0 };
        let current = match attr(bar.0, "AXValue").and_then(|v| number(v.0)) {
            Some(v) => range * v.clamp(0.0, 1.0),
            None => a0 - c0,
        };
        let value = ((current + delta).clamp(0.0, range) / range).clamp(0.0, 1.0);
        let n = Cf(unsafe { CFNumberCreate(std::ptr::null(), kCFNumberFloat64Type, &value as *const f64 as _) } as _);
        moved |= set_attr(bar.0, "AXValue", n.0).is_ok();
    }
    if !moved && settable(area, "AXMakeScreenRectVisible") {
        let r = [t.0, t.1, t.2, t.3];
        let v = Cf(unsafe { AXValueCreate(3, r.as_ptr() as _) }); // kAXValueCGRectType
        moved = set_attr(area, "AXMakeScreenRectVisible", v.0).is_ok();
    }
    moved
}

/// Chess board squares are buttons whose centers can hit a piece instead. When the center does
/// not reach the button, click the centroid of the cells of a 64 x 64 grid over its frame that
/// hit exactly it.
fn chess_point(e: AXRef) -> Option<(f64, f64)> {
    use objc2_app_kit::NSRunningApplication;
    let p = pid(e)?;
    let bundle = NSRunningApplication::runningApplicationWithProcessIdentifier(p)?.bundleIdentifier()?;
    if attr_str(e, "AXRole").as_deref() != Some("AXButton") || bundle.to_string() != "com.apple.Chess" { return None; }
    // Original only grid-searches squares whose label ends in a square name like "a1".
    let sq: Vec<char> = attr_str(e, "AXDescription").unwrap_or_default().chars().rev().take(2).collect();
    if sq.len() < 2 || !('1'..='8').contains(&sq[0]) || !('a'..='h').contains(&sq[1]) { return None; }
    let (x, y, w, h) = frame(e)?;
    if contains_hit(e, p, crate::input::Pt { x: x + w / 2.0, y: y + h / 2.0 }) { return None; }
    let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
    for j in 0..64 {
        for i in 0..64 {
            let (px, py) = (x + w / 64.0 * (i as f64 + 0.5), y + h / 64.0 * (j as f64 + 0.5));
            if element_at(p, px, py).is_some_and(|hit| unsafe { CFEqual(hit.0, e) } != 0) {
                (sx, sy, n) = (sx + px, sy + py, n + 1.0);
            }
        }
    }
    (n > 0.0).then(|| (sx / n, sy / n))
}

pub fn frame(e: AXRef) -> Option<(f64, f64, f64, f64)> {
    let (mut p, mut s) = ([0f64; 2], [0f64; 2]);
    let pos = attr(e, "AXPosition")?;
    let size = attr(e, "AXSize")?;
    unsafe {
        (AXValueGetValue(pos.0, 1, p.as_mut_ptr() as _)
            && AXValueGetValue(size.0, 2, s.as_mut_ptr() as _))
        .then_some((p[0], p[1], s[0], s[1]))
    }
}

pub fn set_selected_range(e: AXRef, loc: usize, len: usize) -> Result<(), String> {
    let range = [loc as isize, len as isize];
    let v = Cf(unsafe { AXValueCreate(4, range.as_ptr() as _) });
    set_attr(e, "AXSelectedTextRange", v.0)
}

pub fn window(app: AXRef) -> Option<Cf> {
    attr(app, "AXFocusedWindow")
        .or_else(|| attr(app, "AXMainWindow"))
        .or_else(|| {
            attr(app, "AXWindows")
                .and_then(|w| array(&w).first().map(|&e| Cf(unsafe { CFRetain(e) })))
        })
}

/// The menu bar item whose menu is open, if any.
pub fn open_menu(app: AXRef) -> Option<Cf> {
    let bar = attr(app, "AXMenuBar")?;
    let items = attr(bar.0, "AXChildren")?;
    let items = array(&items);
    items.into_iter().find(|&i| matches!(attr_str(i, "AXSelected").as_deref(), Some("on" | "1"))).map(|i| Cf(unsafe { CFRetain(i) }))
}

/// A rendered tree: the text, and per index the element and its line.
#[derive(Default)]
pub struct Tree {
    /// Web areas (AXWebArea) that have content, to tell when a browser has built its tree.
    pub web_areas: usize,
    pub text: String,
    pub elems: Vec<Cf>,
    pub lines: Vec<String>,
}


/// Attributes read for every element, in one round trip.
const ATTRS: [&str; 21] = [
    "AXRole", "AXSubrole", "AXRoleDescription", "AXTitle", "AXDescription", "AXValue", "AXPlaceholderValue",
    "AXHelp", "AXURL", "AXDocument", "AXIdentifier", "AXValueDescription", "AXSelected", "AXFocused", "AXEnabled",
    "AXDisclosing", "AXChildren", "AXVerticalScrollBar", "AXHorizontalScrollBar", "AXRows", "AXVisibleRows",
];

thread_local! {
    static ATTR_ARRAY: Cf = {
        let names: Vec<Cf> = ATTRS.iter().map(|a| cfstr(a)).collect();
        let ptrs: Vec<CFTypeRef> = names.iter().map(|n| n.0).collect();
        Cf(unsafe { CFArrayCreate(null(), ptrs.as_ptr() as _, ptrs.len() as _, &kCFTypeArrayCallBacks) } as _)
    };
}

/// One element as the original's tree transforms see it.
struct Node {
    elem: Cf,
    role: String,
    subrole: String,
    role_desc: Option<String>,
    title: Option<String>,
    desc: Option<String>,
    value: Option<String>,
    /// "string", "boolean", "integer" or "float": the CF type of AXValue.
    value_type: Option<&'static str>,
    placeholder: Option<String>,
    help: Option<String>,
    url: Option<String>,
    id: Option<String>,
    details: Option<String>,
    selectable: bool,
    selected: bool,
    focusable: bool,
    focused: bool,
    enabled: bool,
    value_settable: bool,
    /// AXDisclosing, only where it is settable (the original's "disclosable").
    disclosing: Option<bool>,
    /// Checkboxes and switches, which survive row flattening.
    boolean: bool,
    /// Labels listed as Secondary Actions.
    actions: Vec<String>,
    /// Some action survives filtering besides AXShowMenu and AXScrollToVisible.
    interesting: bool,
    /// Listed row ranges and the row count, when only the visible part of a long table is shown.
    subset: Option<(Vec<(usize, usize)>, usize)>,
    /// AXServesAsTitleForUIElements of a label.
    serves: Vec<Cf>,
    children: Vec<Node>,
}

fn is_true(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("on" | "1"))
}

fn filled(v: &Option<String>) -> bool {
    v.as_ref().is_some_and(|v| !v.trim().is_empty())
}

/// Read an element and (up to `max_depth`) its descendants. None for what the original never
/// shows: the Apple menu and menu separators.
fn fetch(e: AXRef, depth: usize, max_depth: usize, budget: &mut usize, browser: bool) -> Option<Node> {
    *budget = budget.saturating_sub(1);
    let mut vals: CFArrayRef = std::ptr::null();
    ATTR_ARRAY.with(|names| unsafe { AXUIElementCopyMultipleAttributeValues(e, names.0 as _, 0, &mut vals) });
    let vals = Cf(vals as _);
    let got = if vals.0.is_null() { vec![] } else { array(&vals) };
    // Missing attributes come back as AXValue error objects.
    let raw = |i: usize| got.get(i).copied().filter(|v| !v.is_null() && unsafe { CFGetTypeID(*v) != AXValueGetTypeID() });
    let text = |i: usize| raw(i).and_then(to_string).filter(|s| !s.is_empty());
    let kids = |i: usize| raw(i).map(|v| array(&Cf(unsafe { CFRetain(v) }))).unwrap_or_default();
    let role = text(0).unwrap_or_default();
    let title = text(3);
    if role == "AXMenuBarItem" && title.as_deref() == Some("Apple") {
        return None;
    }
    let children_raw = if depth < max_depth { kids(16) } else { vec![] };
    if role == "AXMenuItem" && title.is_none() && children_raw.is_empty() {
        return None;
    }
    let value_type = raw(5).and_then(|v| unsafe {
        let t = CFGetTypeID(v);
        if t == CFStringGetTypeID() {
            Some("string")
        } else if t == CFBooleanGetTypeID() {
            Some("boolean")
        } else if t == CFNumberGetTypeID() {
            Some(if CFNumberIsFloatType(v as _) != 0 { "float" } else { "integer" })
        } else {
            None
        }
    });
    let subrole = text(1).unwrap_or_default();
    // Text areas show their styled text as Markdown; toggle buttons read as on/off.
    let value = match (role.as_str(), subrole.as_str(), text(5)) {
        ("AXTextArea", ..) => crate::text::markdown(e).map(|m| m.text).filter(|t| !t.is_empty()),
        (_, "AXToggle" | "AXSwitch", Some(v)) if v == "0" || v == "1" => Some(if v == "1" { "on" } else { "off" }.into()),
        (.., v) => v,
    };
    // Labels never read as settable; in browsers, where Chromium reports a settable AXValue on
    // everything, only text inputs and toggles do.
    let text_input = matches!(role.as_str(), "AXTextField" | "AXTextArea");
    let toggle = value_type == Some("boolean");
    let value_settable =
        role != "AXStaticText" && (!browser || text_input || toggle) && raw(5).is_some() && settable(e, "AXValue");
    let (vertical, horizontal) = (raw(17).is_some(), raw(18).is_some());
    let closed_menu = role == "AXMenuBarItem" && depth >= max_depth;
    // The original's filterActions, then only "other" actions are listed: presses, menus and
    // scroll-to-visible are implied by the role.
    let kept: Vec<(String, String)> = actions(e)
        .into_iter()
        .filter(|(n, _)| match n.as_str() {
            _ if closed_menu => false,
            "AXShowAlternateUI" | "AXShowDefaultUI" => false,
            "AXCancel" => role != "AXMenuBar" && role != "AXMenuItem",
            "AXPick" => role != "AXMenuItem",
            "AXScrollLeftByPage" | "AXScrollRightByPage" => horizontal,
            "AXScrollUpByPage" | "AXScrollDownByPage" => vertical,
            "AXConfirm" => role != "AXTextField",
            "AXIncrement" | "AXDecrement" => !value_settable,
            _ => true,
        })
        .collect();
    let interesting = kept.iter().any(|(n, _)| !matches!(n.as_str(), "AXShowMenu" | "AXScrollToVisible"));
    let mut labels: Vec<String> = kept
        .into_iter()
        .filter(|(n, _)| !matches!(n.as_str(), "AXPress" | "AXShowMenu" | "AXScrollToVisible"))
        .map(|(_, l)| l)
        .collect();
    let disclosing = (raw(15).is_some() && settable(e, "AXDisclosing")).then(|| is_true(&text(15)));
    // Disclosable rows offer the toggle (see perform_secondary_action).
    labels.extend(disclosing.map(|on| if on { "Collapse" } else { "Expand" }.to_string()));
    // Tables under 500 rows list every row; longer ones only the visible rows, noted as a subset.
    let rows = kids(19);
    let (children_raw, subset) = if depth >= max_depth || rows.is_empty() {
        (children_raw, None)
    } else if rows.len() < 500 {
        (rows, None)
    } else {
        let visible = kids(20);
        let mut ranges: Vec<(usize, usize)> = vec![];
        for &v in &visible {
            let i = attr_str(v, "AXIndex").and_then(|i| i.parse().ok())
                .or_else(|| rows.iter().position(|&r| unsafe { CFEqual(r, v) } != 0)).unwrap_or(0);
            match ranges.last_mut() {
                Some(r) if r.1 == i => r.1 = i + 1,
                _ => ranges.push((i, i + 1)),
            }
        }
        // The original counts at most the first 100 rows of a long table.
        (visible, Some((ranges, rows.len().min(100))))
    };
    let focused_raw = raw(13).is_some();
    let mut children = vec![];
    for &c in &children_raw {
        if *budget == 0 || depth >= 60 {
            break;
        }
        children.extend(fetch(c, depth + 1, max_depth, budget, browser));
    }
    let id = text(10).filter(|i| !i.starts_with('_') && !i.starts_with("NS"));
    let menu = role.starts_with("AXMenu");
    Some(Node {
        elem: Cf(unsafe { CFRetain(e) }),
        subrole,
        role_desc: text(2),
        title,
        desc: text(4),
        value,
        value_type,
        placeholder: text(6),
        help: text(7),
        url: text(8).or_else(|| text(9)),
        id,
        details: text(11),
        // Menus track selection for highlighting only; the original never shows it.
        selectable: !menu && raw(12).is_some() && settable(e, "AXSelected"),
        selected: !menu && is_true(&text(12)),
        focusable: focused_raw && settable(e, "AXFocused"),
        focused: is_true(&text(13)),
        enabled: !matches!(text(14).as_deref(), Some("off" | "0")),
        value_settable,
        disclosing,
        boolean: value_type == Some("boolean") || role == "AXCheckBox",
        actions: labels,
        interesting,
        subset,
        serves: if role == "AXStaticText" { attr(e, "AXServesAsTitleForUIElements").map(|a| array(&a).into_iter().map(|x| Cf(unsafe { CFRetain(x) })).collect()).unwrap_or_default() } else { vec![] },
        children,
        role,
    })
}

impl Node {
    fn descendants(&self) -> Vec<&Node> {
        let mut out = vec![];
        for c in &self.children {
            out.push(c);
            out.extend(c.descendants());
        }
        out
    }

    fn any_below(&self, f: impl Fn(&Node) -> bool) -> bool {
        self.descendants().into_iter().any(f)
    }

    /// The original's `flattened`: this node's and every descendant's trimmed text, one per line.
    fn flattened(&self, k: fn(&Node) -> &Option<String>) -> Option<String> {
        let parts: Vec<&str> = std::iter::once(self).chain(self.descendants()).filter_map(|n| k(n).as_deref()).map(str::trim).filter(|t| !t.is_empty()).collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    fn descriptive(&self) -> bool {
        [&self.title, &self.desc, &self.value, &self.placeholder].into_iter().any(filled) || self.role == "AXImage"
    }

    /// Descriptive counting the whole subtree.
    fn descriptive_deep(&self) -> bool {
        let k: [fn(&Node) -> &Option<String>; 4] = [|n| &n.title, |n| &n.desc, |n| &n.value, |n| &n.placeholder];
        k.into_iter().any(|k| self.flattened(k).is_some()) || self.role == "AXImage"
    }

    fn can_interact(&self) -> bool {
        match self.role.as_str() {
            "AXStaticText" => self.focusable || self.selectable,
            "AXTextArea" | "AXGroup" => self.focusable,
            _ => true,
        }
    }

    fn role_word(&self) -> String {
        let r = self.role.as_str();
        match (&self.role_desc, r) {
            (_, "AXMenuBarItem" | "AXMenuItem" | "AXRadioGroup") => String::new(),
            (_, "AXLink") => "link".into(),
            (None, "AXMenu") => "menu".into(),
            (None, "AXMenuBar") => "menu bar".into(),
            (None, "AXGenericElement") => "element".into(),
            (None, _) => "unknown".into(),
            (Some(_), "AXRow") => "row".into(),
            (Some(_), "AXGroup") => "container".into(),
            (Some(d), _) => d.clone(),
        }
    }

    /// The element's line without its index, as the original renders it.
    fn line(&self) -> String {
        let subset = self.subset.as_ref().map(|(ranges, n)| {
            let r: Vec<String> = ranges.iter().map(|(a, b)| format!("{a}-{b}")).collect();
            format!("(showing {} of {n} items)", r.join(", "))
        });
        [self.description(), subset.unwrap_or_default()].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
    }

    /// Role word, flags and fields: the line minus the row-subset note.
    fn description(&self) -> String {
        let flags: Vec<String> = [
            (self.selectable && !self.selected, "selectable".to_string()),
            (self.selected, "selected".into()),
            (!self.enabled && !matches!(self.role.as_str(), "AXGroup" | "AXStaticText"), "disabled".into()),
            (self.disclosing.is_some(), if self.disclosing == Some(true) { "expanded" } else { "collapsed" }.into()),
            (self.value_settable && self.value_type.is_some(), match self.value_type {
                Some("string") | None => "settable".into(),
                Some(t) => format!("settable, {t}"),
            }),
        ]
        .into_iter()
        .filter_map(|(on, f)| on.then_some(f))
        .collect();
        let candidates = [
            (None, self.title.clone()),
            (Some("Description"), self.desc.clone()),
            (Some("URL"), self.url.as_deref().filter(|_| self.role != "AXImage").map(|u| short_url(u).to_string())),
            (Some("Help"), self.help.clone()),
            (Some("Value"), self.value.clone()),
            (Some("Details"), self.details.clone()),
            (Some("Placeholder"), self.placeholder.clone()),
            (Some("ID"), self.id.clone()),
            (Some("Secondary Actions"), (!self.actions.is_empty()).then(|| self.actions.join(", "))),
        ];
        // A field repeating an earlier field's text is left out.
        let mut seen = std::collections::HashSet::new();
        let mut fields: Vec<(Option<&str>, String)> = candidates
            .into_iter()
            .filter_map(|(k, v)| v.filter(|v| !v.is_empty() && seen.insert(v.clone())).map(|v| (k, v)))
            // The original cuts each field at 512 characters.
            .map(|(k, v)| (k, v.chars().take(512).collect::<String>()))
            .collect();
        // Long or multi-line fields go last so the short ones stay readable.
        fields.sort_by_key(|(_, v)| v.chars().count() > 100 || v.contains('\n'));
        let fields = match fields.as_slice() {
            [(k, v)] if *k != Some("Secondary Actions") => v.clone(),
            _ => fields.iter().map(|(k, v)| k.map_or(v.clone(), |k| format!("{k}: {v}"))).collect::<Vec<_>>().join(", "),
        };
        let flags = (!flags.is_empty()).then(|| format!("({})", flags.join(", "))).unwrap_or_default();
        [self.role_word(), flags, fields].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
    }
}

/// The original's transform pipeline for one node, top-down: the node is transformed with its
/// raw children, then each child in turn. A replacement goes through the whole pipeline again.
fn transform(mut n: Node) -> Option<Node> {
    // associateTitleUIElements: a label titling one sibling becomes that sibling's title; a
    // label titling several holds them as its children.
    let mut kids = std::mem::take(&mut n.children);
    let mut i = 0;
    while i < kids.len() {
        let mut served: Vec<usize> = kids[i].serves.iter().filter_map(|s| kids.iter().position(|k| unsafe { CFEqual(k.elem.0, s.0) } != 0)).filter(|&j| j != i).collect();
        served.sort();
        if served.len() == 1 && kids[served[0]].title.is_none() {
            let label = kids.remove(i);
            let j = if served[0] > i { served[0] - 1 } else { served[0] };
            kids[j].title = label.value;
            continue;
        }
        if served.len() > 1 {
            let before = served.iter().filter(|&&j| j < i).count();
            let mut moved: Vec<Node> = vec![];
            for j in served.into_iter().rev() { moved.insert(0, kids.remove(j)); }
            i -= before;
            kids[i].children.extend(moved);
        }
        i += 1;
    }
    n.children = kids;
    // removeElementsUnderCalendarEvents
    if n.role_desc.as_deref() == Some("Event") {
        n.children.clear();
    }
    // flattenLinksIntoMarkdownText: a link reads as one line, its target as the value.
    if n.role == "AXLink" {
        if n.title.as_deref().unwrap_or("").is_empty() { n.title = n.flattened(|n| &n.title); }
        if n.value.is_none() { n.value = n.url.take().map(|u| short_url(&u).to_string()); }
        n.children.retain(|c| c.role == "AXLink"); // nested links stay, the rest is the link's text
    }
    // flattenIntoSelectableAncestor: a selectable row-like element without editable or
    // selectable parts reads as one line; only its checkboxes stay as children.
    if n.selectable && !n.focused && n.role != "AXTable" && n.subrole != "AXTableRow" && !n.any_below(|d| d.value_settable || d.selectable) {
        // Checkboxes and disclosure triangles stay as children and add no text to the line.
        fn pull(n: &mut Node, out: &mut Vec<Node>) {
            for mut c in std::mem::take(&mut n.children) {
                if c.boolean || c.role == "AXDisclosureTriangle" {
                    c.children.clear();
                    out.push(c);
                } else {
                    pull(&mut c, out);
                    n.children.push(c);
                }
            }
        }
        let mut kept = vec![];
        pull(&mut n, &mut kept);
        n.title = n.flattened(|n| &n.title);
        n.desc = n.flattened(|n| &n.desc);
        n.value = n.flattened(|n| &n.value);
        n.children = kept;
    }
    // pruneNonDescriptiveSubtrees
    let reachable = |d: &Node| d.focusable || d.selectable || d.interesting;
    if !reachable(&n) && !n.descriptive_deep() && !n.any_below(reachable) {
        return None;
    }
    // pruneEmptyDisabledElements
    if !n.enabled && !n.focused && n.children.is_empty() && !n.descriptive_deep() {
        return None;
    }
    // mergeSingleItemGroups: a plain group shows its only child in its place, whatever
    // actions, identifier or help it has.
    let plain = !n.selectable && !n.focused && !n.descriptive();
    if plain && n.role == "AXGroup" {
        if n.children.len() == 1 && n.children[0].subset.is_none() {
            return transform(n.children.pop().unwrap());
        }
        if n.children.is_empty() {
            return None;
        }
    }
    // flattenRedundantHierarchy: a child that would print the same line as its parent is
    // replaced by its own children.
    if plain {
        let line = n.line();
        let kids = std::mem::take(&mut n.children);
        for c in kids {
            if c.role == n.role && c.subset.is_none() && c.line() == line {
                n.children.extend(c.children);
            } else {
                n.children.push(c);
            }
        }
    }
    // flattenRepetitiveStaticText: labels repeating the parent's own text add nothing.
    if !n.can_interact() && !n.focusable && !n.selectable && !n.any_below(Node::can_interact) {
        let own: Vec<&str> = [&n.desc, &n.value].into_iter().filter_map(|v| v.as_deref()).map(str::trim).collect();
        let own: Vec<String> = own.into_iter().map(String::from).collect();
        n.children.retain(|c| {
            let t = c.desc.as_ref().or(c.value.as_ref()).map(|t| t.trim().to_string());
            !(c.role == "AXStaticText" && c.children.is_empty() && t.is_some_and(|t| own.contains(&t)))
        });
    }
    // mergeTextOnlySiblings: runs of sibling labels read as one line of text.
    if !n.selectable && !n.focusable {
        let mut merged: Vec<Node> = vec![];
        let mut run = false;
        for c in std::mem::take(&mut n.children) {
            let text = c.role == "AXStaticText" && c.value.is_some();
            match merged.last_mut() {
                Some(last) if text && run => {
                    let v = last.value.take().unwrap_or_default();
                    last.value = Some(format!("{v} {}", c.value.unwrap_or_default()));
                }
                _ => merged.push(c),
            }
            run = text;
        }
        n.children = merged;
    }
    n.children = std::mem::take(&mut n.children).into_iter().filter_map(transform).collect();
    Some(n)
}

fn emit(n: Node, depth: usize, tree: &mut Tree) {
    if n.role == "AXWebArea" && !n.children.is_empty() {
        tree.web_areas += 1;
    }
    let line = n.line();
    // Something with nothing to say (a radio group) gives its place to its children.
    let depth = if line.is_empty() {
        depth
    } else {
        let i = tree.elems.len();
        tree.text += &format!("{}{i} {line}\n", "\t".repeat(depth));
        // Selection and focus notes quote the line without the subset note.
        tree.lines.push(format!("{i} {}", n.description()));
        tree.elems.push(n.elem);
        depth + 1
    };
    for c in n.children {
        emit(c, depth, tree);
    }
}

/// Render the element's tree in the Computer Use text format, indented `depth` tabs and read
/// `max_depth` levels down.
pub fn render(root: AXRef, depth: usize, max_depth: usize, tree: &mut Tree) {
    let mut budget = MAX_NODES;
    let browser = pid(root)
        .and_then(|p| objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(p)?.bundleIdentifier())
        .is_some_and(|b| crate::apps::BROWSERS.contains(&b.to_string().as_str()))
        || pid(root).and_then(|p| objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(p)?.bundleURL()?.path())
            .is_some_and(|p| std::path::Path::new(&format!("{p}/Contents/Frameworks/Electron Framework.framework")).exists());
    if let Some(n) = fetch(root, 0, max_depth, &mut budget, browser).and_then(transform) {
        emit(n, depth, tree);
    }
}
