//! Clean-room MCP stdio server exposing the Codex Computer Use tool interface on macOS.
mod apps;
mod ax;
mod cursor;
mod focus;
mod input;
mod menu;
mod overlay;
mod preventer;
mod service;
mod setup;
mod text;

use ax::Cf;
use base64::Engine;
use core_foundation_sys::array::*;
use core_foundation_sys::dictionary::CFDictionaryGetValue;
use core_foundation_sys::number::{kCFNumberFloat64Type, kCFNumberSInt32Type, CFNumberGetValue};
use input::Pt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::process::Command;

/// What the last get_app_state saw, so later calls can address elements and pixels.
struct Snapshot {
    taken: std::time::Instant,
    tree: ax::Tree,
    window: (f64, f64, f64, f64),
    window_id: u32,
    image: (f64, f64),
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFArrayRef;
    fn CGSessionCopyCurrentDictionary() -> *const std::ffi::c_void;
}

/// JSON-RPC, one message per line.
struct Io {
    lines: std::io::Lines<Box<dyn BufRead + Send>>,
    out: Box<dyn Write + Send>,
}

impl Io {
    fn next(&mut self) -> Option<Value> {
        loop {
            if let Ok(m) = serde_json::from_str(&self.lines.next()?.ok()?) {
                return Some(m);
            }
        }
    }

    fn send(&mut self, v: Value) {
        let _ = writeln!(self.out, "{v}");
        let _ = self.out.flush();
    }
}

struct Server {
    io: Io,
    snaps: HashMap<i32, Snapshot>,
    enforcers: HashMap<i32, focus::Enforcer>,
}

fn main() {
    if std::env::args().any(|a| a == "--probe-screen") {
        std::process::exit(if setup::screen_recording_now() { 0 } else { 1 });
    }
    let setup = std::env::args().any(|a| a == "--setup");
    let bundle = service::bundle();
    if let Some(app) = bundle.as_ref().filter(|_| !service::launched_by_launch_services()) {
        // Started by the MCP host (or a shell): the app does the work, we relay.
        if setup {
            std::process::exit(if service::launch(app, "--setup") { 0 } else { 1 });
        }
        if let Err(e) = service::relay(app) {
            eprintln!("rust-computer-use: {e}");
            std::process::exit(1);
        }
        std::process::exit(0);
    }
    input::warm(); // keyboard layout lookups must happen on the main thread
    // Opened from Finder (no arguments) the app walks through the permissions.
    let service = std::env::args().any(|a| a == "--service");
    if setup || (bundle.is_some() && !service) {
        // Just the permission walkthrough; the process ends when its window closes.
        overlay::run(|| {
            setup::show(true);
            loop {
                std::thread::park();
            }
        })
    }
    if service {
        overlay::run(|| service::listen(serve))
    }
    overlay::run(|| serve(Box::new(std::io::BufReader::new(std::io::stdin())), Box::new(std::io::stdout())))
}

fn serve(input: Box<dyn BufRead + Send>, out: Box<dyn Write + Send>) {
    let mut srv = Server {
        io: Io { lines: input.lines(), out },
        snaps: HashMap::new(),
        enforcers: HashMap::new(),
    };
    while let Some(msg) = srv.io.next() {
        let Some(id) = msg.get("id").cloned().filter(|_| msg.get("method").is_some()) else {
            continue; // notifications and stray replies need no answer
        };
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => {
                json!({
                    "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "rust-computer-use", "version": env!("CARGO_PKG_VERSION")},
                })
            }
            "tools/list" => json!({"tools": tools()}),
            "tools/call" => {
                let (name, args) = (msg["params"]["name"].as_str().unwrap_or(""), &msg["params"]["arguments"]);
                match call(&mut srv, name, args) {
                    Ok(content) => json!({"content": content}),
                    Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                }
            }
            "ping" => json!({}),
            m => {
                let error = json!({"code": -32601, "message": format!("unknown method {m}")});
                srv.io.send(json!({"jsonrpc": "2.0", "id": id, "error": error}));
                continue;
            }
        };
        srv.io.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }
    ax::restore_web();
    srv.enforcers.clear();
    let recorded: Vec<i32> = MENUS.lock().unwrap().iter().map(|m| m.0).collect();
    for pid in recorded { record_menu(pid, None); }
}

/// Tool schemas, worded exactly as the original advertises them.
fn tools() -> Value {
    let app = json!({"type": "string", "description": "App name, full app path, or unambiguous bundle identifier"});
    let idx = json!({"type": "string", "description": "Element identifier"});
    let str_ = |d: &str| json!({"type": "string", "description": d});
    let num = |d: &str| json!({"type": "number", "description": d});
    // Only the two readers are hinted read-only and idempotent; every hint is spelled out.
    let tool = |name: &str, desc: &str, props: Value, req: &[&str]| {
        let ro = matches!(name, "list_apps" | "get_app_state");
        let mut schema = json!({"type": "object", "properties": props, "additionalProperties": false});
        if !req.is_empty() {
            schema["required"] = json!(req);
        }
        let hints = json!({"readOnlyHint": ro, "idempotentHint": ro, "destructiveHint": false, "openWorldHint": false});
        json!({"name": name, "description": desc, "inputSchema": schema, "annotations": hints})
    };
    json!([
        tool("list_apps", "List the apps on this computer. Returns the set of apps that are currently running, as well as any that have been used in the last 14 days, including details on usage frequency", json!({}), &[]),
        tool("get_app_state", "Start an app use session if needed, then get the state of the app's key window and return a screenshot and accessibility tree. This must be called once per assistant turn before interacting with the app", json!({"app": app}), &["app"]),
        tool("click", "Click an element by index or pixel coordinates from screenshot", json!({"app": app, "element_index": str_("Element index to click"),
            "x": num("X coordinate in screenshot pixel coordinates"), "y": num("Y coordinate in screenshot pixel coordinates"),
            "click_count": {"type": "integer", "description": "Number of clicks. Defaults to 1"},
            "mouse_button": {"type": "string", "enum": ["left", "right", "middle"], "description": "Mouse button to click. Defaults to left."}}), &["app"]),
        tool("perform_secondary_action", "Invoke a secondary accessibility action exposed by an element", json!({"app": app, "element_index": idx,
            "action": str_("Secondary accessibility action name")}), &["app", "element_index", "action"]),
        tool("set_value", "Set the value of a settable accessibility element", json!({"app": app, "element_index": idx, "value": str_("Value to assign")}), &["app", "element_index", "value"]),
        tool("select_text", "Select text inside a text element, or place the text cursor before or after it. Provide text exactly as it appears in the accessibility tree, including any Markdown formatting. If the text is not unique, provide surrounding prefix or suffix text to disambiguate it.",
            json!({"app": str_("App name or bundle identifier"), "element_index": str_("Text element identifier"),
            "text": str_("Target text as shown in the accessibility tree"),
            "prefix": str_("Optional text immediately before the target, used to disambiguate repeated matches"),
            "suffix": str_("Optional text immediately after the target, used to disambiguate repeated matches"),
            "selection": {"type": "string", "enum": ["text", "cursor_before", "cursor_after"], "description": "Whether to select the text or place the cursor before or after it. Defaults to text."}}),
            &["app", "element_index", "text"]),
        tool("scroll", "Scroll an element in a direction by a number of pages", json!({"app": app, "element_index": idx,
            "direction": str_("Scroll direction: up, down, left, or right"),
            "pages": num("Number of pages to scroll. Fractional values are supported. Defaults to 1")}), &["app", "element_index", "direction"]),
        tool("drag", "Drag from one point to another using pixel coordinates", json!({"app": app, "from_x": num("Start X coordinate"),
            "from_y": num("Start Y coordinate"), "to_x": num("End X coordinate"), "to_y": num("End Y coordinate")}), &["app", "from_x", "from_y", "to_x", "to_y"]),
        tool("press_key", "Press a key or key-combination on the keyboard, including modifier and navigation keys.\n  - This supports xdotool's `key` syntax.\n  - Examples: \"a\", \"Return\", \"Tab\", \"super+c\", \"Up\", \"KP_0\" (for the numpad 0 key).",
            json!({"app": app, "key": str_("Key or key combination to press")}), &["app", "key"]),
        tool("type_text", "Type literal text using keyboard input", json!({"app": app, "text": str_("Literal text to type")}), &["app", "text"]),
    ])
}

fn text(s: impl Into<String>) -> Value {
    json!([{"type": "text", "text": s.into()}])
}

/// Failures inside an action, in the original's wording.
fn server_err(m: impl std::fmt::Display) -> String {
    format!("Computer Use server error -10005: {m}")
}

/// Argument checks the original makes before touching the app.
fn validate(name: &str, a: &Value) -> Result<(), String> {
    let schema = tools().as_array().unwrap().iter().find(|t| t["name"] == name).cloned();
    let schema = schema.ok_or(format!("Unknown tool: {name}"))?;
    let mut required: Vec<&str> = schema["inputSchema"]["required"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).collect();
    if name == "click" && a.get("element_index").is_none() {
        required.extend(["x", "y"]);
    }
    if let Some(k) = required.into_iter().find(|k| a.get(*k).is_none()) {
        return Err(format!("Missing required argument: {k}"));
    }
    if let Some(b) = a["mouse_button"].as_str().filter(|b| !["left", "right", "middle"].contains(b)) {
        return Err(format!("Invalid mouse button: {b}"));
    }
    if let Some(d) = a["direction"].as_str().filter(|d| !["up", "down", "left", "right"].contains(d)) {
        return Err(format!("Invalid scroll direction: {d}"));
    }
    Ok(())
}

/// Accessibility and Screen Recording. When either is missing the setup window opens; while
/// it is up (for 100 s, the original's grant timeout) calls report the grant as pending.
fn permissions() -> Result<(), String> {
    static OPENED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if setup::all_granted() {
        return Ok(());
    }
    let mut opened = OPENED.lock().unwrap();
    if setup::is_open() && opened.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(100)) {
        return Err("Computer Use permissions are still pending. The user has not finished granting Accessibility and Screen Recording permissions in the Rust Computer Use window. Call this tool again, as the user is almost done finishing granting permissions. Do not end your turn yet, just call this tool again.".into());
    }
    setup::show(false);
    *opened = Some(std::time::Instant::now());
    Err("Computer Use permissions are not granted".into())
}

fn screen_locked() -> bool {
    let d = Cf(unsafe { CGSessionCopyCurrentDictionary() });
    !d.0.is_null() && {
        let v = unsafe { CFDictionaryGetValue(d.0 as _, ax::cfstr("CGSSessionScreenIsLocked").0) };
        !v.is_null() && ax::to_string(v).as_deref() == Some("on")
    }
}

fn call(srv: &mut Server, name: &str, a: &Value) -> Result<Value, String> {
    validate(name, a)?;
    if overlay::stopped() {
        return Ok(text("This application session has been explicitly stopped by the user for this turn. Stop your work and send a final message noting they stopped the session and you're ready to continue if they want you to. Computer Use can be used again in the next assistant turn."));
    }
    if name == "list_apps" {
        return Ok(text(apps::list()));
    }
    permissions()?;
    if screen_locked() {
        return Err("The Mac is locked. Unlock it before reading accessibility text.".into());
    }
    let q = a["app"].as_str().unwrap_or("");
    let app = apps::resolve(q)?;
    if name == "get_app_state" {
        return state(&mut srv.snaps, &app, true);
    }
    let Some(snap) = srv.snaps.get(&app.pid) else {
        return Ok(text(format!("Computer Use is not active for '{q}'. You first must call `get_app_state` to get the latest state before doing other Computer Use actions. If `get_app_state` is not available in your environment, use `tool_search` to surface it.")));
    };
    // Like the original, refuse to act while the user is using the app, and after they used it
    // since the last get_app_state. Changes the app makes by itself don't count.
    if let Some(t) = overlay::touched(app.pid) {
        let wait = USER_DEBOUNCE.saturating_sub(t.elapsed());
        if !wait.is_zero() {
            return Err(format!("The user is still interacting with '{}'. Wait {} seconds and retry.", app.name, wait.as_secs_f64().ceil()));
        }
        if t > snap.taken {
            return Err(format!("The user changed '{}'. Re-query the latest state with `get_app_state` before sending more actions.", app.name));
        }
    }
    input::target(app.pid, snap.window_id, Pt { x: snap.window.0, y: snap.window.1 });
    if !srv.enforcers.contains_key(&app.pid) {
        srv.enforcers.insert(app.pid, focus::Enforcer::new(app.pid).map_err(server_err)?);
    }
    act(snap, name, a, &app, &srv.enforcers[&app.pid])?;
    // Actions answer with the resulting state once the UI settles, as the original does.
    settle(app.pid);
    state(&mut srv.snaps, &app, false)
        .or_else(|_| Ok(text("Action completed. Call `get_app_state` to fetch the updated UI state.")))
}

/// The menu the original treats as open per app: a menu bar item it "clicked" without pressing
/// it (the controller's open-menu box), so the real menu never appears on screen.
static MENUS: std::sync::Mutex<Vec<(i32, usize)>> = std::sync::Mutex::new(Vec::new());

fn recorded_menu(pid: i32) -> Option<Cf> {
    MENUS.lock().unwrap().iter().find(|m| m.0 == pid).map(|m| Cf(unsafe { core_foundation_sys::base::CFRetain(m.1 as _) }))
}

fn record_menu(pid: i32, item: Option<&Cf>) {
    let mut menus = MENUS.lock().unwrap();
    if let Some(i) = menus.iter().position(|m| m.0 == pid) {
        drop(Cf(menus.remove(i).1 as _));
    }
    if let Some(item) = item {
        menus.push((pid, unsafe { core_foundation_sys::base::CFRetain(item.0) } as usize));
    }
}

/// openMenu(of:): a menu bar item with an AXMenu child.
fn menu_of(e: &Cf) -> bool {
    ax::attr_str(e.0, "AXRole").as_deref() == Some("AXMenuBarItem")
        && ax::attr(e.0, "AXChildren").is_some_and(|c| ax::elements(&c).iter().any(|&m| ax::attr_str(m, "AXRole").as_deref() == Some("AXMenu")))
}

fn act(snap: &Snapshot, name: &str, a: &Value, app: &apps::App, enforcer: &focus::Enforcer) -> Result<(), String> {
    // Any action other than opening a menu leaves the recorded one behind.
    record_menu(app.pid, None);
    let index = a["element_index"].as_str().unwrap_or("").trim().to_string();
    let elem = || -> Result<&Cf, String> {
        let i: usize = index.parse().map_err(|_| server_err(format!("{index} is an invalid element ID")))?;
        snap.tree.elems.get(i).ok_or(server_err(format!("{index} is an invalid element ID")))
    };
    let to_screen = |x: f64, y: f64| {
        let (wx, wy, ww, wh) = snap.window;
        Pt { x: wx + x * ww / snap.image.0, y: wy + y * wh / snap.image.1 }
    };
    let center = |e: &Cf| {
        ax::frame(e.0).map(|(x, y, w, h)| Pt { x: x + w / 2.0, y: y + h / 2.0 }).ok_or(server_err("element has no frame"))
    };
    let f = |k: &str| a[k].as_f64().ok_or(format!("Missing required argument: {k}"));
    let catalyst = ax::attr(ax::app_element(app.pid).0, "AXExhibitsMacCatalystBehavior").is_some_and(|v| ax::to_bool(v.0));
    // The original's focus enforcer: whenever the app no longer believes it has focus, it is
    // made to believe so with the user's app kept in front and the screen frozen.
    let enforce = || -> Result<(), String> {
        if enforcer.believes_focus() {
            return Ok(());
        }
        input::set_mouse_flags(0);
        let win = ax::window(ax::app_element(app.pid).0);
        let point = win.and_then(|w| input::ax_point(w.0, "AXActivationPoint"));
        enforcer.enforce(point).map_err(server_err)?;
        Ok(())
    };
    // Rule B of the original's sendClick: activate only for clicks inside web views or on text
    // that may get selected; other clicks carry Command so AppKit leaves the app in the back.
    let prepare_click = |el: Option<&Cf>, at: Pt| -> Result<(), String> {
        let hit = ax::element_at(app.pid, at.x, at.y);
        let web = el.or(hit.as_ref()).is_some_and(|e| ax::in_web_view(e.0));
        if enforcer.believes_focus() {
            input::set_mouse_flags(0);
        } else if web || hit.as_ref().is_some_and(|e| ax::may_select(e.0)
            || (catalyst && ax::attr_str(e.0, "AXRole").as_deref() == Some("AXMenuButton"))) {
            enforce()?;
        } else {
            input::set_mouse_flags(0x100000);
        }
        Ok(())
    };

    match name {
        "click" => {
            let button = a["mouse_button"].as_str().unwrap_or("left");
            let count = a["click_count"].as_i64().unwrap_or(1);
            // The cursor glides to the spot first; the click goes out once it is close enough.
            let w = snap.window_id;
            if a.get("element_index").is_some() {
                let e = elem()?;
                // The original's menu bar pre-step: make the app believe it is active and record
                // the item's menu as open instead of pressing it, so nothing shows on screen.
                if button == "left" && count == 1 && menu_of(e) {
                    enforce()?;
                    record_menu(app.pid, Some(e));
                    return Ok(());
                }
                // clickablePoint(): the frame center if the clipping ancestors leave it visible.
                // Chrome can expose an AXActivationPoint outside the element entirely.
                ax::scroll_to_visible(e.0);
                let point = ax::clickable_point(e.0).map(|(x, y)| Pt { x, y });
                let at = point.unwrap_or(center(e)?);
                if point.is_some() { cursor::move_to(at, w); }
                let actions = ax::actions(e.0);
                let action = ["AXPick", "AXPress"].into_iter().find(|name| actions.iter().any(|(n, _)| n == name));
                let press = button == "left" && count == 1 && !(ax::in_web_view(e.0) && ax::contains_hit(e.0, app.pid, at));
                if press && action.is_some() && catalyst && !enforcer.believes_focus()
                    && (ax::may_select(e.0) || ax::attr_str(e.0, "AXRole").as_deref() == Some("AXMenuButton")) { enforce()?; }
                if press && action.is_some_and(|a| ax::perform(e.0, a).is_ok()) {
                    cursor::press(1);
                    return Ok(());
                }
                let at = point.ok_or(server_err("cannotClickOffscreenElement"))?;
                prepare_click(Some(e), at)?;
                click_with_cursor(at, button, count, false);
            } else {
                let at = to_screen(f("x")?, f("y")?);
                let inside = |(x, y, w, h): (f64, f64, f64, f64)| at.x >= x && at.x < x + w && at.y >= y && at.y < y + h;
                if !windows(app.pid, true).into_iter().any(|(_, b)| inside(b)) && !inside(snap.window) {
                    return Err(server_err(format!("windowNotFoundAtPosition(({:?}, {:?}))", at.x, at.y)));
                }
                cursor::move_to(at, w);
                prepare_click(None, at)?;
                click_with_cursor(at, button, count, true);
            }
        }
        "drag" => {
            let (from, to) = (to_screen(f("from_x")?, f("from_y")?), to_screen(f("to_x")?, f("to_y")?));
            cursor::move_to(from, snap.window_id);
            prepare_click(None, from)?;
            input::drag(from, to);
            cursor::move_to(to, snap.window_id);
        }
        "perform_secondary_action" => {
            let e = elem()?;
            let want = a["action"].as_str().unwrap_or("");
            let invalid = || server_err(format!("{want} is not a valid secondary action for {index}"));
            // Collapse/Expand are synthesised for outline rows from AXDisclosing.
            if let ("Collapse" | "Expand", Some(_)) = (want, ax::attr_str(e.0, "AXDisclosing")) {
                return ax::set_bool(e.0, "AXDisclosing", want == "Expand").map_err(server_err);
            }
            let action = ax::actions(e.0).into_iter().find(|(n, d)| d == want || n == want).ok_or_else(invalid)?;
            if action.0 == "AXPick" && menu_of(e) {
                enforce()?;
                record_menu(app.pid, Some(e));
                return Ok(());
            }
            ax::perform(e.0, &action.0).map_err(server_err)?;
        }
        "press_key" => {
            enforce()?;
            input::press(a["key"].as_str().unwrap_or("")).map_err(|k| server_err(format!("keyNotFound(\"{k}\")")))?
        }
        "type_text" => {
            enforce()?;
            input::type_text(a["text"].as_str().unwrap_or(""));
        }
        "set_value" => {
            let e = elem()?;
            if !ax::settable(e.0, "AXValue") {
                return Err(server_err("Cannot set a value for an element that is not settable"));
            }
            // focusFieldIfNeeded: focus the field, clicking it when AXFocused does not take.
            if ax::settable(e.0, "AXFocused") && ax::attr(e.0, "AXFocused").is_some_and(|v| !ax::to_bool(v.0))
                && (ax::set_bool(e.0, "AXFocused", true).is_err() || !ax::attr(e.0, "AXFocused").is_some_and(|v| ax::to_bool(v.0))) {
                act(snap, "click", &json!({"element_index": index}), app, enforcer)?;
            }
            ax::set_attr(e.0, "AXValue", ax::cfstr(a["value"].as_str().unwrap_or("")).0).map_err(server_err)?;
            // Search fields and address bars are submitted with Return, as the original does.
            let search = ax::attr_str(e.0, "AXSubrole").as_deref() == Some("AXSearchField")
                || ax::attr_str(e.0, "AXIdentifier").as_deref() == Some("WEB_BROWSER_ADDRESS_AND_SEARCH_FIELD")
                || ax::attr_str(e.0, "AXKeyShortcutsValue").as_deref() == Some("⌘L");
            if search {
                enforce()?;
                input::press("Return").map_err(|k| server_err(format!("keyNotFound(\"{k}\")")))?;
            }
        }
        "scroll" => {
            let e = elem()?;
            let dir = a["direction"].as_str().unwrap_or("down");
            let pages = a["pages"].as_f64().unwrap_or(1.0);
            if pages <= 0.0 {
                return Err("pages must be > 0".into());
            }
            // One pixel scroll of pages x the element's extent at its center, as the original
            // sends (it never uses the AX page actions).
            let (_, _, w, h) = ax::frame(e.0).ok_or(server_err("element has no frame"))?;
            let px = |size: f64| (pages * size).round() as i32;
            let (dy, dx) = match dir {
                "up" => (-px(h), 0),
                "down" => (px(h), 0),
                "left" => (0, -px(w)),
                _ => (0, px(w)),
            };
            cursor::move_to(center(e)?, snap.window_id);
            input::set_mouse_flags(0);
            input::scroll(center(e)?, dy, dx);
        }
        "select_text" => {
            let e = elem()?;
            if !ax::settable(e.0, "AXSelectedTextRange") {
                return Err(server_err("Cannot select text for an element that does not support a settable selected text range"));
            }
            // Match against the Markdown shown in the tree, then map back to plain offsets.
            let md = text::markdown(e.0).unwrap_or_else(|| text::markdown_of(String::new()));
            let target = a["text"].as_str().unwrap_or("");
            let (pre, suf) = (a["prefix"].as_str().unwrap_or(""), a["suffix"].as_str().unwrap_or(""));
            let needle = format!("{pre}{target}{suf}");
            let mut hits = md.text.match_indices(&needle).map(|(i, _)| i + pre.len());
            let start = hits.next().ok_or(server_err(format!("Text not found: {target}")))?;
            if hits.next().is_some() {
                return Err(server_err(format!("Text is not unique: {target}. Provide a prefix or suffix")));
            }
            let (lo, hi) = md.plain_range(start, start + target.len());
            match a["selection"].as_str().unwrap_or("text") {
                "cursor_before" => ax::set_selected_range(e.0, lo, 0),
                "cursor_after" => ax::set_selected_range(e.0, hi, 0),
                _ => ax::set_selected_range(e.0, lo, hi - lo),
            }
            .map_err(server_err)?;
        }
        _ => return Err(format!("Unknown tool: {name}")),
    }
    Ok(())
}

/// The original's build number, which its state header reports.
const CUA_VERSION: u32 = 1001501;
/// How long after the user's last input the app counts as in use. The original's value is set
/// where its binary has no symbols; this is a guess.
const USER_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Wait for the app to settle like the original: watch its AX notifications, polling every
/// 0.1 s, until 0.25 s pass without one (at most 5 s).
fn settle(pid: i32) {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let start = std::time::Instant::now();
    let since = |t: &std::time::Instant| t.elapsed().as_millis() as u64;
    extern "C" fn noted(_o: *const std::ffi::c_void, _e: *const std::ffi::c_void, _n: core_foundation_sys::string::CFStringRef, start: *mut std::ffi::c_void) {
        let start = unsafe { &*(start as *const std::time::Instant) };
        LAST.store(start.elapsed().as_millis() as u64, Relaxed);
    }
    LAST.store(0, Relaxed);
    let names = ["AXTitleChanged", "AXLayoutChanged", "AXFocusedWindowChanged", "AXFocusedUIElementChanged", "AXElementBusyChanged",
        "AXMenuOpened", "AXSheetCreated", "AXWindowCreated", "AXCreated", "AXUIElementDestroyed"];
    let observer = ax::observe(pid, &names, noted, &start as *const _ as _);
    while since(&start) < 5000 && since(&start).saturating_sub(LAST.load(Relaxed)) < 250 {
        ax::run_loop_for(0.1);
    }
    drop(observer);
}

/// Post the click while the cursor shows it being pressed.
fn click_with_cursor(at: Pt, button: &str, count: i64, hover: bool) {
    let shown = std::thread::spawn(move || cursor::press(count));
    input::click(at, button, count, hover);
    let _ = shown.join();
}

const SELECTION_NOTE: &str = "Note: Pay special attention to the content selected by the user. If the user asks a question or refers to the content they are looking at on-screen, they might be referring to the selected content (but they might be referring to something else that's visible, too).";

/// The key window's tree plus menu bar. It ends with what the user has selected in the
/// focused element or, failing that, the focused element itself, like the original.
fn render(app: &apps::App) -> Result<(ax::Tree, Cf), String> {
    let root = ax::app_element(app.pid);
    let win = ax::window(root.0).ok_or(server_err(format!("'{}' has no open window", app.name)))?;
    // A minimized window is restored (without activating the app), as the original does.
    if ax::attr_str(win.0, "AXMinimized").as_deref() == Some("on") && ax::set_bool(win.0, "AXMinimized", false).is_ok() {
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
    let mut tree = ax::Tree::default();
    // While a menu is open the original shows only that menu, rooted at its menu bar item.
    if let Some(menu) = recorded_menu(app.pid).or_else(|| ax::open_menu(root.0)) {
        ax::render(menu.0, 0, usize::MAX, &mut tree);
        return Ok((tree, win));
    }
    ax::render(win.0, 0, usize::MAX, &mut tree);
    // Chromium and Firefox build their web trees lazily (first request, or after dropping it
    // in the background): give it up to 3 s to show page content, and a nearly empty window too.
    let web = apps::web_engine(&app.path) && ax::enable_web(app.pid);
    for _ in 0..12 {
        if (!web || tree.web_areas > 0) && tree.elems.len() >= 8 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        tree = ax::Tree::default();
        ax::render(win.0, 0, usize::MAX, &mut tree);
    }
    if let Some(bar) = ax::attr(root.0, "AXMenuBar") {
        ax::render(bar.0, 0, 1, &mut tree);
    }
    let index_of = |e: *const std::ffi::c_void| {
        tree.elems.iter().position(|x| unsafe { core_foundation_sys::base::CFEqual(x.0, e) } != 0)
    };
    // Focus inside another window (Catalyst keeps it behind a sheet) is not reported, nor is
    // Catalyst's root content group (observed behavior; the original's check wasn't located).
    let focused = ax::attr(root.0, "AXFocusedUIElement").filter(|f| {
        ax::attr(f.0, "AXWindow").is_none_or(|w| unsafe { core_foundation_sys::base::CFEqual(w.0, win.0) } != 0)
            && ax::attr_str(f.0, "AXSubrole").as_deref() != Some("iOSContentGroup")
    });
    let selected: Vec<usize> = focused
        .as_ref()
        .and_then(|f| ax::attr(f.0, "AXSelectedRows"))
        .map(|rows| ax::elements(&rows).into_iter().filter_map(index_of).collect())
        .unwrap_or_default();
    if !selected.is_empty() {
        // Only the index, role and flags: the row's text is already in the tree above.
        let heads: Vec<String> = selected.iter().map(|&i| {
            let l = &tree.lines[i];
            l.find(") ").map_or(l.clone(), |p| l[..=p].to_string())
        }).collect();
        tree.text += &format!("\nSelected:\n\t{}\n\n{SELECTION_NOTE}\n", heads.join("\n\t"));
    } else if let Some(t) = focused.as_ref().and_then(|f| ax::attr_str(f.0, "AXSelectedText")) {
        tree.text += &format!("\nSelected text: ```\n{t}\n```\n\n{SELECTION_NOTE}\n");
    } else if let Some(i) = focused.as_ref().and_then(|f| index_of(f.0)) {
        tree.text += &format!("\nThe focused UI element is {}\n", tree.lines[i]);
    } else {
        tree.text += "\n"; // the tree always ends with a blank line
    }
    Ok((tree, win))
}

const SPOTIFY_NOTE: &str = "Note: In order to be usable, Spotify app links must be rewritten as regular links (e.g. use open.spotify.com instead of xpui.app.spotify.com). Only use Spotify links that are written verbatim in the UI above. Note that IDs are only valid with their associated type (e.g. you cannot change an \"album\" URL to a \"track\" URL).";

/// The original's per-app notes: one for browsers, and a file per app named after it.
fn instructions(app: &apps::App) -> Option<&'static str> {
    if apps::BROWSERS.contains(&app.bundle_id.as_str()) {
        return Some("## Browser Computer Use\n\nWhen navigating to a new website or starting a separate web task, prefer opening a new tab instead of reusing the current tab; reuse the current tab only when the user explicitly asks to continue there or when the current page is clearly the right place to continue the existing workflow.\n");
    }
    Some(match app.bundle_id.as_str() {
        "com.apple.Music" => include_str!("../instructions/AppleMusic.md"),
        "com.apple.clock" => include_str!("../instructions/Clock.md"),
        "notion.id" => include_str!("../instructions/Notion.md"),
        "com.apple.iWork.Numbers" => include_str!("../instructions/Numbers.md"),
        "com.tinyspeck.slackmacgap" => include_str!("../instructions/Slack.md"),
        "com.spotify.client" => include_str!("../instructions/Spotify.md"),
        "com.apple.ScreenContinuity" => include_str!("../instructions/iPhone Mirroring.md"),
        _ => return None,
    })
}

/// get_app_state gets the full framing; actions get the shorter one.
fn state(snaps: &mut HashMap<i32, Snapshot>, app: &apps::App, full: bool) -> Result<Value, String> {
    let (tree, win) = render(app)?;
    let frame = ax::frame(win.0).ok_or(server_err("window has no frame"))?;
    // The AX window's own number; a frame match alone can pick a same-sized popup (Chrome's omnibox).
    let window_id = ax::window_id(win.0).or_else(|| window_id(app.pid, frame)).unwrap_or(0);
    // The window server's title first (Chrome's omits the app name), then the AX title.
    let title = window_name(window_id).or_else(|| ax::attr_str(win.0, "AXTitle")).unwrap_or_default();
    let note = if app.bundle_id == "com.spotify.client" { format!("\n{SPOTIFY_NOTE}\n") } else { String::new() };
    let window = format!("Window: \"{title}\", App: {}.\n{}{note}", app.name, tree.text);
    let body = if full {
        let tips = instructions(app).map(|t| format!("<app_specific_instructions>\n{t}\n</app_specific_instructions>\n")).unwrap_or_default();
        format!(
            "Computer Use state (CUA App Version: {CUA_VERSION})\n{tips}<app_state>\nApp={}/ (bundleID {}, pid {})\n{window}</app_state>",
            app.path, app.bundle_id, app.pid
        )
    } else {
        format!("App={} (pid {})\n{window}", app.bundle_id, app.pid)
    };
    let mut content = vec![json!({"type": "text", "text": body})];
    let mut image = (frame.2, frame.3);
    let menu_open = recorded_menu(app.pid).is_some() || ax::open_menu(ax::app_element(app.pid).0).is_some(); // no screenshot then
    match screenshot(window_id) {
        Ok(_) if menu_open => {}
        Ok((jpeg, size)) => {
            image = size;
            let data = base64::engine::general_purpose::STANDARD.encode(jpeg);
            content.push(json!({"type": "image", "data": data, "mimeType": "image/jpeg"}));
        }
        Err(e) => content.push(json!({"type": "text", "text": format!("Screenshot unavailable: {e}")})),
    }
    snaps.insert(app.pid, Snapshot { taken: std::time::Instant::now(), tree, window: frame, window_id, image });
    Ok(Value::Array(content))
}

/// On-screen windows front to back, desktop excluded, as (number, owner pid, layer).
pub fn window_list() -> Vec<(u32, i32, i64)> {
    let list = Cf(unsafe { CGWindowListCopyWindowInfo(1 | 16, 0) } as _);
    let num = |d: *const std::ffi::c_void, k: &str| {
        let v = unsafe { CFDictionaryGetValue(d as _, ax::cfstr(k).0) };
        let mut n = 0i64;
        if !v.is_null() {
            unsafe { CFNumberGetValue(v as _, core_foundation_sys::number::kCFNumberSInt64Type, &mut n as *mut i64 as _) };
        }
        n
    };
    (0..unsafe { CFArrayGetCount(list.0 as _) })
        .map(|i| unsafe { CFArrayGetValueAtIndex(list.0 as _, i) })
        .map(|d| (num(d, "kCGWindowNumber") as u32, num(d, "kCGWindowOwnerPID") as i32, num(d, "kCGWindowLayer")))
        .collect()
}

/// The app's normal (layer 0) windows as (window number, CG bounds x, y, w, h).
/// `on_screen` leaves out hidden windows and other Spaces.
pub fn windows(pid: i32, on_screen: bool) -> Vec<(u32, (f64, f64, f64, f64))> {
    let list = Cf(unsafe { CGWindowListCopyWindowInfo(if on_screen { 1 | 16 } else { 16 }, 0) } as _);
    let num = |d: *const std::ffi::c_void, k: &str| {
        let v = unsafe { CFDictionaryGetValue(d as _, ax::cfstr(k).0) };
        let mut f = f64::NAN;
        if !v.is_null() {
            unsafe { CFNumberGetValue(v as _, kCFNumberFloat64Type, &mut f as *mut f64 as _) };
        }
        f
    };
    (0..unsafe { CFArrayGetCount(list.0 as _) })
        .map(|i| unsafe { CFArrayGetValueAtIndex(list.0 as _, i) })
        .filter(|&d| num(d, "kCGWindowOwnerPID") as i32 == pid && num(d, "kCGWindowLayer") == 0.0)
        .map(|d| {
            let b = unsafe { CFDictionaryGetValue(d as _, ax::cfstr("kCGWindowBounds").0) };
            let bounds = (num(b, "X"), num(b, "Y"), num(b, "Width"), num(b, "Height"));
            (num(d, "kCGWindowNumber") as u32, bounds)
        })
        .collect()
}

/// The process owning a window.
pub fn window_owner(id: u32) -> Option<i32> {
    let list = Cf(unsafe { CGWindowListCopyWindowInfo(8, id) } as _); // kCGWindowListOptionIncludingWindow
    if list.0.is_null() || unsafe { CFArrayGetCount(list.0 as _) } == 0 {
        return None;
    }
    let d = unsafe { CFArrayGetValueAtIndex(list.0 as _, 0) };
    let v = unsafe { CFDictionaryGetValue(d as _, ax::cfstr("kCGWindowOwnerPID").0) };
    let mut pid = 0i32;
    (!v.is_null() && unsafe { CFNumberGetValue(v as _, kCFNumberSInt32Type, &mut pid as *mut i32 as _) }).then_some(pid)
}

/// The window server's name for a window (kCGWindowName), if it has one.
fn window_name(id: u32) -> Option<String> {
    let list = Cf(unsafe { CGWindowListCopyWindowInfo(8, id) } as _); // kCGWindowListOptionIncludingWindow
    if list.0.is_null() || unsafe { CFArrayGetCount(list.0 as _) } == 0 {
        return None;
    }
    let d = unsafe { CFArrayGetValueAtIndex(list.0 as _, 0) };
    let v = unsafe { CFDictionaryGetValue(d as _, ax::cfstr("kCGWindowName").0) };
    (!v.is_null()).then(|| ax::to_string(v)).flatten()
}

/// CGWindowID of the app's window closest to the AX window frame. All windows count, so
/// hidden apps and other Spaces still capture.
fn window_id(pid: i32, frame: (f64, f64, f64, f64)) -> Option<u32> {
    let dist = |(x, y, w, h): (f64, f64, f64, f64)| ((x - frame.0).abs() + (y - frame.1).abs() + (w - frame.2).abs() + (h - frame.3).abs()) as i64;
    windows(pid, false).into_iter().min_by_key(|w| dist(w.1)).map(|w| w.0)
}

/// Full-resolution JPEG of one window, as the original sends it.
fn screenshot(id: u32) -> Result<(Vec<u8>, (f64, f64)), String> {
    if id == 0 {
        return Err("no on-screen window".into());
    }
    let path = std::env::temp_dir().join(format!("rust-computer-use-{id}.jpg"));
    let p = path.to_str().unwrap();
    let ok = Command::new("/usr/sbin/screencapture").args(["-x", "-o", "-t", "jpg", &format!("-l{id}"), p]).status();
    if !ok.is_ok_and(|s| s.success()) {
        return Err("screencapture failed (Screen Recording permission?)".into());
    }
    let jpeg = std::fs::read(&path).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&path);
    let size = jpeg_size(&jpeg).ok_or("unreadable JPEG")?;
    Ok((jpeg, size))
}

/// Read pixel dimensions from the JPEG SOF marker.
fn jpeg_size(b: &[u8]) -> Option<(f64, f64)> {
    let mut i = 2;
    while i + 9 < b.len() {
        let (marker, len) = (b[i + 1], u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize);
        if (0xC0..=0xC3).contains(&marker) {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
            return Some((w as f64, h as f64));
        }
        i += 2 + len;
    }
    None
}
