//! Synthetic mouse and keyboard events (CGEvent FFI) with xdotool key-name parsing.
use crate::ax::Cf;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};
use std::{thread::sleep, time::Duration};

// Events go to one process and window, never through the global HID tap, so the
// target app stays in the background and the user's cursor and focus are untouched.
static PID: AtomicI32 = AtomicI32::new(0);
static WINDOW: AtomicU32 = AtomicU32::new(0);
static ORIGIN: Mutex<Pt> = Mutex::new(Pt { x: 0.0, y: 0.0 });

type Ev = *const c_void;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateMouseEvent(src: Ev, ty: u32, p: Pt, button: u32) -> Ev;
    fn CGEventCreateKeyboardEvent(src: Ev, key: u16, down: bool) -> Ev;
    fn CGEventCreateScrollWheelEvent2(src: Ev, units: u32, count: u32, y: i32, x: i32, z: i32) -> Ev;
    fn CGEventKeyboardSetUnicodeString(e: Ev, len: usize, s: *const u16);
    fn CGEventSetFlags(e: Ev, flags: u64);
    fn CGEventSetLocation(e: Ev, p: Pt);
    fn CGEventSetWindowLocation(e: Ev, p: Pt);
    fn CGEventSetIntegerValueField(e: Ev, field: u32, v: i64);
    fn CGEventPostToPid(pid: i32, e: Ev);
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;
    fn TISGetInputSourceProperty(src: *const c_void, key: *const c_void) -> *const c_void;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    fn LMGetKbdType() -> u8;
    #[allow(clippy::too_many_arguments)]
    fn UCKeyTranslate(
        layout: *const u8, code: u16, action: u16, mods: u32, kbd: u32, opts: u32,
        dead: *mut u32, max: usize, len: *mut usize, chars: *mut u16,
    ) -> i32;
}

/// Apps told they are active, so they can be told otherwise when the agent moves on.
static BELIEVERS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

/// Make a background app believe it is the active app without activating it, as the
/// original's focus enforcer does: an AppKit-defined ApplicationActivated event posted to the
/// process. AppKit only honors key equivalents and modifier navigation (super+s,
/// shift+alt+Left) in an app that believes it is active. Any other app told so earlier gets
/// ApplicationDeactivated first.
pub fn believe_active(pid: i32) {
    let mut b = BELIEVERS.lock().unwrap();
    if b.contains(&pid) {
        return;
    }
    for other in b.drain(..) {
        post_app_event(other, 2);
    }
    post_app_event(pid, 1);
    b.push(pid);
}

/// Undo believe_active for every app (at the end of the session).
pub fn disbelieve_all() {
    for pid in BELIEVERS.lock().unwrap().drain(..) {
        post_app_event(pid, 2);
    }
}

fn post_app_event(pid: i32, subtype: i16) {
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
    let ev = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::AppKitDefined, objc2_foundation::NSPoint::ZERO, NSEventModifierFlags(0), 0.0, 0, None, subtype, 0, 0,
    );
    if let Some(ev) = ev {
        let cg: *const c_void = unsafe { objc2::msg_send![&*ev, CGEvent] };
        if !cg.is_null() {
            unsafe { CGEventPostToPid(pid, cg) };
        }
    }
}

/// `origin` is the window's top-left corner in global points.
pub fn target(pid: i32, window: u32, origin: Pt) {
    PID.store(pid, Relaxed);
    WINDOW.store(window, Relaxed);
    *ORIGIN.lock().unwrap() = origin;
}

fn post(e: Ev) {
    let e = Cf(e);
    unsafe { CGEventPostToPid(PID.load(Relaxed), e.0) };
    sleep(Duration::from_millis(8));
}

/// Pointer events keep a human pace: SwiftUI drops presses whose down and up arrive within a
/// few milliseconds. The original spaces them about 50-65 ms apart.
fn post_mouse(e: Ev) {
    post(e);
    sleep(Duration::from_millis(45));
}

/// Address a pointer event to the target window. AppKit drops pid-posted clicks on a
/// background window unless they carry its window number (field 51, plus 91/92) and a
/// window-local location, matching what the original service sends.
fn address(e: Ev, p: Pt) -> Ev {
    let o = *ORIGIN.lock().unwrap();
    for field in [51, 91, 92] {
        unsafe { CGEventSetIntegerValueField(e, field, WINDOW.load(Relaxed) as i64) };
    }
    unsafe { CGEventSetWindowLocation(e, Pt { x: p.x - o.x, y: p.y - o.y }) };
    e
}

fn mouse(ty: u32, p: Pt, button: u32) -> Ev {
    address(unsafe { CGEventCreateMouseEvent(std::ptr::null(), ty, p, button) }, p)
}

pub fn click(p: Pt, button: &str, count: i64) {
    let (down, up, b) = match button {
        "right" => (3, 4, 1),
        "middle" => (25, 26, 2),
        _ => (1, 2, 0),
    };
    post_mouse(mouse(5, p, 0));
    for n in 1..=count.max(1) {
        for ty in [down, up] {
            let e = mouse(ty, p, b);
            unsafe { CGEventSetIntegerValueField(e, 1, n) }; // kCGMouseEventClickState
            post_mouse(e);
        }
    }
}

pub fn drag(from: Pt, to: Pt) {
    post_mouse(mouse(1, from, 0));
    for i in 1..=20 {
        let t = i as f64 / 20.0;
        post(mouse(6, Pt { x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t }, 0));
    }
    post_mouse(mouse(2, to, 0));
}

/// Scroll by points (positive = down / right). Pixel units avoid per-app line acceleration;
/// small steps keep apps that clamp large deltas honest.
pub fn scroll(at: Pt, dy: i32, dx: i32) {
    let steps = (dy.abs().max(dx.abs()) as f64 / 40.0).ceil().max(1.0) as i32;
    let mut sent = (0, 0);
    for i in 1..=steps {
        let step = (dy * i / steps - sent.0, dx * i / steps - sent.1);
        sent = (sent.0 + step.0, sent.1 + step.1);
        let e = unsafe { CGEventCreateScrollWheelEvent2(std::ptr::null(), 0, 2, -step.0, -step.1, 0) };
        unsafe { CGEventSetLocation(e, at) };
        post(address(e, at));
    }
}

pub fn type_text(text: &str) {
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            key_event(36, 0);
        }
        // One event per character: many apps read only the first unit of a multi-char event.
        for ch in line.chars() {
            if let Some(&(code, flags)) = layout().get(&ch) {
                key_event(code, flags);
                continue;
            }
            let mut buf = [0u16; 2];
            let units = ch.encode_utf16(&mut buf);
            for down in [true, false] {
                let e = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), 0, down) };
                unsafe { CGEventKeyboardSetUnicodeString(e, units.len(), units.as_ptr()) };
                post(e);
            }
        }
    }
}

fn key_event(code: u16, flags: u64) {
    for down in [true, false] {
        let e = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), code, down) };
        unsafe { CGEventSetFlags(e, flags) };
        post(e);
    }
}

/// Press an xdotool-style combo such as "super+shift+t" or "KP_0". Errors name the unknown key.
pub fn press(combo: &str) -> Result<(), String> {
    let mut flags = 0u64;
    let parts: Vec<&str> = combo.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last().ok_or(String::new())?;
    for m in mods {
        flags |= match m.to_lowercase().as_str() {
            "shift" => 0x20000,
            "ctrl" | "control" => 0x40000,
            "alt" | "option" => 0x80000,
            "super" | "cmd" | "command" | "meta" => 0x100000,
            "fn" => 0x800000,
            _ => return Err(m.to_string()),
        };
    }
    let (code, extra) = keycode(key).ok_or(key.to_string())?;
    key_event(code, flags | extra);
    Ok(())
}

pub fn warm() {
    layout();
}

/// Character -> (keycode, event flags) for the current keyboard layout, like the original's
/// UCKeyTranslate lookup: the same eight shift/option/control combinations in the same order.
/// Built once; main-row keys win over keypad duplicates.
fn layout() -> &'static HashMap<char, (u16, u64)> {
    static MAP: OnceLock<HashMap<char, (u16, u64)>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        unsafe {
            let src = Cf(TISCopyCurrentKeyboardLayoutInputSource());
            let data = TISGetInputSourceProperty(src.0, kTISPropertyUnicodeKeyLayoutData);
            if data.is_null() {
                return map;
            }
            let ptr = core_foundation_sys::data::CFDataGetBytePtr(data as _);
            let kbd = LMGetKbdType() as u32;
            for carbon in [0u32, 0x200, 0x800, 0xa00, 0x1000, 0x1200, 0x1800, 0x1a00] {
                let mods = carbon >> 8;
                let flags = [(0x200, 0x20000), (0x800, 0x80000), (0x1000, 0x40000)]
                    .iter()
                    .filter(|(c, _)| carbon & c != 0)
                    .fold(0u64, |f, (_, cg)| f | cg);
                for code in 0..128u16 {
                    let (mut dead, mut len, mut buf) = (0u32, 0usize, [0u16; 4]);
                    // kUCKeyActionDown, kUCKeyTranslateNoDeadKeysMask
                    UCKeyTranslate(ptr, code, 0, mods, kbd, 1, &mut dead, 4, &mut len, buf.as_mut_ptr());
                    let c = char::decode_utf16(buf[..len].iter().copied()).next().and_then(Result::ok);
                    if let Some(c) = c.filter(|c| !c.is_control()) {
                        map.entry(c).or_insert((code, flags));
                    }
                }
            }
        }
        map
    })
}

/// macOS virtual keycode for an xdotool keysym, plus the modifier flags its character needs.
fn keycode(k: &str) -> Option<(u16, u64)> {
    let mut chars = k.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return layout().get(&c).copied();
    }
    let k = k.to_lowercase();
    // Punctuation keysyms name a character, which sits wherever the current layout puts it.
    const PUNCT: &[(&str, char)] = &[
        ("minus", '-'), ("equal", '='), ("comma", ','), ("period", '.'), ("slash", '/'), ("semicolon", ';'),
        ("apostrophe", '\''), ("bracketleft", '['), ("bracketright", ']'), ("backslash", '\\'), ("grave", '`'),
        ("plus", '+'), ("exclam", '!'), ("at", '@'), ("numbersign", '#'), ("dollar", '$'), ("percent", '%'),
        ("asciicircum", '^'), ("ampersand", '&'), ("asterisk", '*'), ("parenleft", '('), ("parenright", ')'),
        ("underscore", '_'), ("colon", ':'), ("quotedbl", '"'), ("less", '<'), ("greater", '>'), ("question", '?'),
        ("braceleft", '{'), ("braceright", '}'), ("bar", '|'), ("asciitilde", '~'),
    ];
    if let Some(&(_, c)) = PUNCT.iter().find(|(n, _)| *n == k) {
        return layout().get(&c).copied();
    }
    // Function and navigation keys have fixed codes on every layout.
    let named: &[(&str, u16)] = &[
        ("return", 36), ("enter", 36), ("tab", 48), ("space", 49), ("backspace", 51), ("escape", 53), ("esc", 53),
        ("delete", 117), ("home", 115), ("end", 119), ("prior", 116), ("page_up", 116), ("next", 121),
        ("page_down", 121), ("left", 123), ("right", 124), ("down", 125), ("up", 126), ("kp_decimal", 65),
        ("kp_multiply", 67), ("kp_add", 69), ("kp_subtract", 78), ("kp_divide", 75), ("kp_enter", 76),
        ("kp_equal", 81), ("f1", 122), ("f2", 120), ("f3", 99), ("f4", 118), ("f5", 96), ("f6", 97), ("f7", 98),
        ("f8", 100), ("f9", 101), ("f10", 109), ("f11", 103), ("f12", 111),
    ];
    if let Some(d) = k.strip_prefix("kp_").and_then(|d| d.parse::<usize>().ok()).filter(|d| *d < 10) {
        return Some(([82, 83, 84, 85, 86, 87, 88, 89, 91, 92][d], 0));
    }
    named.iter().find(|(n, _)| *n == k).map(|&(_, c)| (c, 0))
}
