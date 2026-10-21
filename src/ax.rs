//! Accessibility tree capture and element actions (AXUIElement FFI).
use core_foundation_sys::array::*;
use core_foundation_sys::base::*;
use core_foundation_sys::number::*;
use core_foundation_sys::string::*;
use std::ffi::c_void;

pub type AXRef = *const c_void;
const MAX_NODES: usize = 2000;

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
    fn AXUIElementCopyAttributeValue(e: AXRef, a: CFStringRef, v: *mut CFTypeRef) -> i32;
    fn AXUIElementSetAttributeValue(e: AXRef, a: CFStringRef, v: CFTypeRef) -> i32;
    fn AXUIElementCopyAttributeNames(e: AXRef, v: *mut CFArrayRef) -> i32;
    fn AXUIElementCopyActionNames(e: AXRef, v: *mut CFArrayRef) -> i32;
    fn AXUIElementCopyActionDescription(e: AXRef, a: CFStringRef, v: *mut CFStringRef) -> i32;
    fn AXUIElementIsAttributeSettable(e: AXRef, a: CFStringRef, out: *mut bool) -> i32;
    fn AXUIElementPerformAction(e: AXRef, a: CFStringRef) -> i32;
    fn AXValueGetValue(v: CFTypeRef, t: u32, out: *mut c_void) -> bool;
    fn AXValueCreate(t: u32, v: *const c_void) -> CFTypeRef;
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
            to_string(core_foundation_sys::url::CFURLGetString(v as _) as _)
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

/// Chromium/Electron only build their web trees when asked. Like the original, use
/// AXManualAccessibility when the app lists it (Electron), else AXEnhancedUserInterface
/// (Chrome). The flag is set on every read: Chrome can drop its web tree while still reporting
/// the flag on, and only a fresh write brings it back. True for apps that take either flag.
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

pub fn attr(e: AXRef, name: &str) -> Option<Cf> {
    let mut v: CFTypeRef = std::ptr::null();
    let n = cfstr(name);
    (unsafe { AXUIElementCopyAttributeValue(e, n.0 as _, &mut v) } == 0 && !v.is_null())
        .then(|| Cf(v))
}
pub fn attr_str(e: AXRef, name: &str) -> Option<String> {
    attr(e, name).and_then(|v| to_string(v.0))
}
/// Type flag for a settable value: "boolean", "integer" or "float"; strings have none.
fn value_kind(e: AXRef) -> Option<&'static str> {
    let v = attr(e, "AXValue")?;
    unsafe {
        let t = CFGetTypeID(v.0);
        if t == CFBooleanGetTypeID() {
            Some("boolean")
        } else if t == CFNumberGetTypeID() {
            Some(if CFNumberIsFloatType(v.0 as _) != 0 { "float" } else { "integer" })
        } else {
            None
        }
    }
}

/// Identifiers AppKit generates itself ("_NS:12", "NSOutlineViewDisclosureButtonKey") say
/// nothing about the app, so the original omits them.
fn appkit_id(id: &str) -> bool {
    id.starts_with("_NS:") || (id.starts_with("NS") && id[2..].starts_with(|c: char| c.is_ascii_uppercase()))
}

/// Web links display without their scheme ("example.com/"); other URLs stay whole.
pub fn short_url(url: &str) -> &str {
    url.strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url)
}
/// What a table row shows: its cells' text as the value (one cell per line), the title and
/// description of a button inside a cell, and its disclosure triangles as the only children.
struct RowParts {
    value: Option<String>,
    title: Option<String>,
    desc: Option<String>,
    triangles: Vec<Cf>,
}

fn row_parts(row: AXRef) -> RowParts {
    fn walk(e: AXRef, text: &mut String, parts: &mut RowParts) {
        let kids = attr(e, "AXChildren");
        for c in kids.as_ref().map(array).unwrap_or_default() {
            match attr_str(c, "AXRole").as_deref() {
                Some("AXStaticText" | "AXTextField") => text.push_str(&attr_str(c, "AXValue").unwrap_or_default()),
                Some("AXDisclosureTriangle") => parts.triangles.push(Cf(unsafe { CFRetain(c) })),
                Some("AXButton") if parts.title.is_none() => {
                    parts.title = attr_str(c, "AXTitle");
                    parts.desc = attr_str(c, "AXDescription").or(parts.desc.take());
                }
                Some("AXImage") if parts.desc.is_none() => parts.desc = attr_str(c, "AXDescription"),
                _ => {}
            }
            walk(c, text, parts);
        }
    }
    let mut parts = RowParts { value: None, title: None, desc: None, triangles: vec![] };
    let kids = attr(row, "AXChildren");
    let mut cells = vec![];
    for cell in kids.as_ref().map(array).unwrap_or_default() {
        let mut text = String::new();
        walk(cell, &mut text, &mut parts);
        if !text.is_empty() {
            cells.push(text);
        }
    }
    parts.value = (!cells.is_empty()).then(|| cells.join("\n"));
    parts
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

fn shown_action(name: &str, role: &str) -> bool {
    // Menus: items show nothing, menus offer Cancel, an open menu bar item Cancel and Pick
    // (render() only descends into a menu bar item while its menu is open).
    match role {
        "AXMenuItem" | "AXMenuBar" => return false,
        "AXMenu" => return name == "AXCancel",
        "AXMenuBarItem" => return matches!(name, "AXCancel" | "AXPick"),
        _ => {}
    }
    match name {
        "AXPress" | "AXShowMenu" | "AXPick" | "AXScrollToVisible" | "AXShowDefaultUI" | "AXShowAlternateUI" => false,
                "AXConfirm" => role != "AXTextField",
        _ => true,
    }
}

/// Follow bare containers (no label, value, identifier or visible actions) that hold exactly
/// one element down to that element.
fn unwrap_group(e: AXRef) -> Cf {
    let mut e = Cf(unsafe { CFRetain(e) });
    loop {
        let bare = attr_str(e.0, "AXRole").as_deref() == Some("AXGroup")
            && ["AXTitle", "AXDescription", "AXValue", "AXHelp"].iter().all(|k| attr_str(e.0, k).is_none())
            && attr_str(e.0, "AXIdentifier").is_none_or(|i| appkit_id(&i))
            && actions(e.0).iter().all(|(n, _)| !shown_action(n, "AXGroup"));
        let kids = attr(e.0, "AXChildren");
        let kids = kids.as_ref().map(array).unwrap_or_default();
        if !bare || kids.len() != 1 {
            return e;
        }
        e = Cf(unsafe { CFRetain(kids[0]) });
    }
}

/// A bare label: static text with no visible actions, description, help or identifier.
fn plain_text(e: AXRef) -> bool {
    attr_str(e, "AXRole").as_deref() == Some("AXStaticText")
        && actions(e).iter().all(|(n, _)| !shown_action(n, "AXStaticText"))
        && ["AXDescription", "AXHelp", "AXTitle"].iter().all(|k| attr_str(e, k).is_none())
        && attr_str(e, "AXIdentifier").is_none_or(|i| appkit_id(&i))
}

/// Render the tree in the Computer Use text format.
pub fn render(root: AXRef, depth: usize, max_depth: usize, tree: &mut Tree) {
    if tree.elems.len() >= MAX_NODES || depth > 60 {
        return;
    }
    let role = attr_str(root, "AXRole").unwrap_or_default();
    let mut title = attr_str(root, "AXTitle");
    if role == "AXMenuBarItem" && title.as_deref() == Some("Apple") {
        return;
    }
    // Only multi-line text areas carry styling worth showing; fields read as plain values.
    let mut value = if role == "AXTextArea" {
        crate::text::markdown(root)
            .map(|m| m.text)
            .filter(|t| !t.is_empty())
    } else {
        attr_str(root, "AXValue")
            .or_else(|| attr_str(root, "AXURL").filter(|_| role == "AXLink"))
            .map(|v| {
                if role == "AXLink" {
                    short_url(&v).to_string()
                } else {
                    v
                }
            })
    };
    let mut desc = attr_str(root, "AXDescription");
    let own_id = attr_str(root, "AXIdentifier").filter(|i| !appkit_id(i));
    if role == "AXStaticText" && title.is_none() && desc.is_none() && own_id.is_none() {
        title = value.take();
    }
    if role == "AXStaticText" && [&title, &value, &desc].iter().all(|v| v.is_none()) {
        return; // an empty label shows nothing
    }
    let row = (role == "AXRow").then(|| row_parts(root));
    if let Some(r) = &row {
        (title, desc, value) = (r.title.clone(), r.desc.clone(), r.value.clone());
    }
    let desc = desc.filter(|d| Some(d) != title.as_ref());
    let help = attr_str(root, "AXHelp");
    let url = attr_str(root, "AXDocument")
        .or_else(|| attr_str(root, "AXURL").filter(|_| role != "AXLink"));
    let id = attr_str(root, "AXIdentifier")
        .filter(|i| Some(i) != desc.as_ref() && !appkit_id(i));
    let (vertical, horizontal) = (attr(root, "AXVerticalScrollBar").is_some(), attr(root, "AXHorizontalScrollBar").is_some());
    let closed_menu = role == "AXMenuBarItem" && depth >= max_depth;
    let mut secondary: Vec<_> = actions(root)
        .into_iter()
        .filter(|_| !closed_menu)
        .filter(|(n, _)| match n.as_str() {
            // Page scrolling shows only along axes that have a scroll bar.
            "AXScrollUpByPage" | "AXScrollDownByPage" => vertical,
            "AXScrollLeftByPage" | "AXScrollRightByPage" => horizontal,
            n => shown_action(n, &role),
        })
        .map(|(_, d)| d)
        .collect();
    // Outline rows get Collapse/Expand, backed by AXDisclosing (see perform_secondary_action).
    let disclosing = attr_str(root, "AXDisclosing");
    match (disclosing.as_deref(), &row) {
        (Some("on"), _) => secondary.push("Collapse".into()),
        (Some("off"), Some(r)) if !r.triangles.is_empty() => secondary.push("Expand".into()),
        _ => {}
    }

    if role == "AXMenuItem" && title.is_none() && attr(root, "AXChildren").is_none_or(|k| array(&k).is_empty()) {
        return; // separator
    }
    let kind = match role.as_str() {
        "AXMenuBarItem" | "AXMenuItem" => None,
        "AXGroup" => Some("container".to_string()),
        "AXLink" => Some("link".to_string()),
        "AXRow" => Some("row".to_string()),
        _ => attr_str(root, "AXRoleDescription").or(Some(role.clone())),
    };
    let mut line =
        format!("{}", tree.elems.len()) + &kind.map(|k| format!(" {k}")).unwrap_or_default();
    // Flags follow the role; settable values also name their type.
    let text_role = matches!(role.as_str(), "AXTextArea" | "AXTextField");
    let value_settable = (value.is_some() || text_role) && settable(root, "AXValue");
    let selected = attr_str(root, "AXSelected");
    // Tables list only the rows on screen, noting the window into the whole list.
    let rows = attr(root, "AXRows").map(|r| array(&r).len()).unwrap_or(0);
    let visible = attr(root, "AXVisibleRows");
    let visible_rows = visible.as_ref().map(array).unwrap_or_default();
    let first = attr(root, "AXRows").and_then(|r| array(&r).iter().position(|&x| visible_rows.first().is_some_and(|&v| unsafe { CFEqual(x, v) } != 0)));
    let showing = (visible.is_some() && visible_rows.len() < rows).then(|| {
        let a = first.unwrap_or(0);
        format!("showing {a}-{} of {rows} items", a + visible_rows.len())
    });
    let flags: Vec<&str> = [
        (selected.as_deref() == Some("on") && !role.starts_with("AXMenu"), Some("selected")),
        (selected.as_deref() == Some("off") && !role.starts_with("AXMenu") && settable(root, "AXSelected"), Some("selectable")),
        (disclosing.as_deref() == Some("on"), Some("expanded")),
        (showing.is_some(), showing.as_deref()),
        (attr_str(root, "AXEnabled").as_deref() == Some("off") && !matches!(role.as_str(), "AXStaticText" | "AXGroup"), Some("disabled")),
        (value_settable, Some("settable")),
        (value_settable, value_kind(root)),
    ]
    .into_iter()
    .filter_map(|(on, f)| f.filter(|_| on))
    .collect();
    if !flags.is_empty() {
        line += &format!(" ({})", flags.join(", "));
    }
    if let Some(t) = &title {
        line += &format!(" {t}");
    }
    let mut fields: Vec<(&str, Option<String>)> =
        vec![("Description", desc), ("Help", help), ("URL", url.map(|u| short_url(&u).to_string()))];
    let details = attr_str(root, "AXValueDescription").filter(|d| Some(d) != value.as_ref());
    let placeholder = attr_str(root, "AXPlaceholderValue");
    fields.extend([("Value", value), ("ID", id)]);
    fields.extend([("Details", details), ("Placeholder", placeholder)]);
    let mut fields: Vec<String> = fields
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| format!("{k}: {v}")))
        .collect();
    // Multi-line fields go last so the short ones stay readable.
    fields.sort_by_key(|f| f.contains('\n'));
    if title.is_none() && fields.len() == 1 && secondary.is_empty() && !fields[0].starts_with("Placeholder") {
        fields[0] = fields[0].split_once(": ").unwrap().1.to_string(); // a lone field prints bare
    }
    if !secondary.is_empty() {
        fields.push(format!("Secondary Actions: {}", secondary.join(", ")));
    }
    if !fields.is_empty() {
        line += &format!(
            "{}{}",
            if title.is_some() { ", " } else { " " },
            fields.join(", ")
        );
    }
    let empty_group = role == "AXGroup" && line.split(' ').count() == 2; // just "N container"
    let kids = attr(root, "AXChildren"); // keeps the array (and its elements) alive
    let children = match (&row, &visible) {
        (Some(r), _) => r.triangles.iter().map(|t| t.0).collect(),
        (_, Some(v)) if rows > 0 => array(v),
        _ => kids.as_ref().map(array).unwrap_or_default(),
    };
    if empty_group && children.len() == 1 {
        // A bare wrapper around one element adds nothing; the original shows the child in its place.
        return render(children[0], depth, max_depth, tree);
    }
    let (text_len, count) = (tree.text.len(), tree.elems.len());
    tree.text += &format!("{}{line}\n", "\t".repeat(depth));
    tree.lines.push(line);
    tree.elems.push(Cf(unsafe { CFRetain(root) }));
    if role == "AXWebArea" && !children.is_empty() {
        tree.web_areas += 1;
    }
    if depth < max_depth {
        // Look through bare one-child wrappers first, as the collapse rule below would.
        let held: Vec<Cf> = children.iter().map(|&c| unwrap_group(c)).collect();
        let children: Vec<CFTypeRef> = held.iter().map(|c| c.0).collect();
        let mut i = 0;
        while i < children.len() {
            // Runs of plain sibling labels read as one line of text, as in the original.
            let run = children[i..].iter().take_while(|&&c| plain_text(c)).count();
            if run > 1 {
                let words: Vec<String> = children[i..i + run].iter().filter_map(|&c| attr_str(c, "AXValue")).collect();
                let line = format!("{} text {}", tree.elems.len(), words.join(" "));
                tree.text += &format!("{}{line}\n", "\t".repeat(depth + 1));
                tree.lines.push(line);
                tree.elems.push(Cf(unsafe { CFRetain(children[i]) }));
                i += run;
            } else {
                render(children[i], depth + 1, max_depth, tree);
                i += 1;
            }
        }
    }
    if empty_group && tree.elems.len() == count + 1 {
        // A container with nothing to show is dropped, as the original does.
        tree.text.truncate(text_len);
        tree.lines.pop();
        tree.elems.pop();
    }
}
