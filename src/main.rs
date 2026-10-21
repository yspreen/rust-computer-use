//! Clean-room MCP stdio server exposing the Codex Computer Use tool interface on macOS.
mod apps;
mod ax;
mod input;
mod overlay;
mod setup;
mod text;

use ax::Cf;
use base64::Engine;
use core_foundation_sys::array::*;
use core_foundation_sys::dictionary::CFDictionaryGetValue;
use core_foundation_sys::number::{kCFNumberFloat64Type, CFNumberGetValue};
use input::Pt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, Write};
use std::process::Command;

/// What the last get_app_state saw, so later calls can address elements and pixels.
struct Snapshot {
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

/// JSON-RPC over stdio. Server-initiated requests (elicitation) wait for their reply and
/// queue anything else that arrives meanwhile.
struct Io {
    lines: std::io::Lines<std::io::StdinLock<'static>>,
    queue: VecDeque<Value>,
    next_id: u64,
}

impl Io {
    fn next(&mut self) -> Option<Value> {
        if let Some(m) = self.queue.pop_front() {
            return Some(m);
        }
        loop {
            if let Ok(m) = serde_json::from_str(&self.lines.next()?.ok()?) {
                return Some(m);
            }
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Option<Value> {
        self.next_id += 1;
        let id = format!("rust-computer-use-{}", self.next_id);
        reply(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m: Value = serde_json::from_str(&self.lines.next()?.ok()?).ok()?;
            if m["id"] == json!(id) && m.get("method").is_none() {
                return Some(m);
            }
            self.queue.push_back(m);
        }
    }
}

struct Server {
    io: Io,
    snaps: HashMap<i32, Snapshot>,
    can_elicit: bool,
    approved: HashSet<String>,
    /// Apps whose instructions this session already delivered; the original sends them once.
    instructed: HashSet<String>,
}

fn approvals_path() -> std::path::PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    std::path::Path::new(&home).join("Library/Application Support/rust-computer-use/approvals.json")
}

fn main() {
    input::warm(); // keyboard layout lookups must happen on the main thread
    if std::env::args().any(|a| a == "--setup") {
        // Just the permission walkthrough; the process ends when its window closes.
        overlay::run(|| {
            setup::show(true);
            loop {
                std::thread::park();
            }
        })
    }
    overlay::run(serve)
}

fn serve() {
    let saved = std::fs::read_to_string(approvals_path()).unwrap_or_default();
    let approved = serde_json::from_str::<Value>(&saved).ok().and_then(|v| {
        v["approvedBundleIdentifiers"].as_array().map(|a| a.iter().filter_map(|b| b.as_str().map(String::from)).collect())
    });
    let mut srv = Server {
        io: Io { lines: std::io::stdin().lock().lines(), queue: VecDeque::new(), next_id: 0 },
        snaps: HashMap::new(),
        can_elicit: false,
        approved: approved.unwrap_or_default(),
        instructed: HashSet::new(),
    };
    while let Some(msg) = srv.io.next() {
        let Some(id) = msg.get("id").cloned().filter(|_| msg.get("method").is_some()) else {
            continue; // notifications and stray replies need no answer
        };
        let result = match msg["method"].as_str().unwrap_or("") {
            "initialize" => {
                srv.can_elicit = msg["params"]["capabilities"].get("elicitation").is_some();
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
                reply(json!({"jsonrpc": "2.0", "id": id, "error": error}));
                continue;
            }
        };
        reply(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }
    ax::restore_web();
    input::disbelieve_all();
}

fn reply(v: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
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

/// Accessibility and Screen Recording. When either is missing, the setup panel opens to walk
/// the user through granting them.
fn permissions() -> Result<(), String> {
    if setup::all_granted() {
        return Ok(());
    }
    setup::show(false);
    let host = setup::host().0;
    Err(format!("Computer Use permissions are not granted. A setup window is open on the Mac: the user needs to allow Accessibility and Screen Recording for {host} there. Call this tool again once they have."))
}

fn screen_locked() -> bool {
    let d = Cf(unsafe { CGSessionCopyCurrentDictionary() });
    !d.0.is_null() && {
        let v = unsafe { CFDictionaryGetValue(d.0 as _, ax::cfstr("CGSSessionScreenIsLocked").0) };
        !v.is_null() && ax::to_string(v).as_deref() == Some("on")
    }
}

/// First use of an app asks the user through MCP elicitation; the answer is remembered.
fn approve(srv: &mut Server, app: &apps::App, q: &str) -> Result<(), String> {
    if srv.approved.contains(&app.bundle_id) || std::env::var_os("RUST_COMPUTER_USE_APPROVE_ALL").is_some() {
        return Ok(());
    }
    if !srv.can_elicit {
        return Err(format!("Computer Use needs the user's approval to use the app '{q}', but this MCP client does not support elicitation. Set RUST_COMPUTER_USE_APPROVE_ALL=1 to allow every app."));
    }
    let params = json!({"message": format!("Allow Rust Computer Use to use {}?", app.name), "requestedSchema": {"type": "object", "properties": {}}});
    let answer = srv.io.request("elicitation/create", params);
    match answer.as_ref().and_then(|r| r["result"]["action"].as_str()) {
        Some("accept") => {
            srv.approved.insert(app.bundle_id.clone());
            let path = approvals_path();
            let mut ids: Vec<&String> = srv.approved.iter().collect();
            ids.sort();
            let saved = std::fs::create_dir_all(path.parent().unwrap())
                .and_then(|_| std::fs::write(&path, json!({"approvedBundleIdentifiers": ids}).to_string()));
            if saved.is_err() {
                eprintln!("Computer Use could not persist the approval permanently for app '{q}'.");
            }
            Ok(())
        }
        Some("decline") => Err(format!("Computer Use approval denied via MCP elicitation for app '{q}'.")),
        _ => Err(format!("Computer Use permission request canceled for app '{q}'.")),
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
        approve(srv, &app, q)?;
        let notes = if srv.instructed.insert(app.bundle_id.clone()) { instructions(&app) } else { String::new() };
        return state(&mut srv.snaps, &app, Some(notes));
    }
    let Some(snap) = srv.snaps.get(&app.pid) else {
        return Ok(text(format!("Computer Use is not active for '{q}'. You first must call `get_app_state` to get the latest state before doing other Computer Use actions. If `get_app_state` is not available in your environment, use `tool_search` to surface it.")));
    };
    // Like the original, refuse to act on a stale view: the agent must look again first.
    if render(&app)?.0.text != snap.tree.text {
        return Err(format!("The user changed '{}'. Re-query the latest state with `get_app_state` before sending more actions.", app.name));
    }
    input::target(app.pid, snap.window_id, Pt { x: snap.window.0, y: snap.window.1 });
    if apps::frontmost() != Some(app.pid) {
        input::believe_active(app.pid);
    }
    act(snap, name, a, &app)?;
    // Actions answer with the resulting state, as the original does.
    std::thread::sleep(std::time::Duration::from_millis(250));
    state(&mut srv.snaps, &app, None)
        .or_else(|_| Ok(text("Action completed. Call `get_app_state` to fetch the updated UI state.")))
}

fn act(snap: &Snapshot, name: &str, a: &Value, app: &apps::App) -> Result<(), String> {
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

    match name {
        "click" => {
            let button = a["mouse_button"].as_str().unwrap_or("left");
            let count = a["click_count"].as_i64().unwrap_or(1);
            let point = if a.get("element_index").is_some() {
                let e = elem()?;
                let pressed = button == "left"
                    && count == 1
                    && ax::actions(e.0).iter().any(|(n, _)| n == "AXPress")
                    && ax::perform(e.0, "AXPress").is_ok();
                if pressed {
                    center(e).map(overlay::point_at).ok();
                }
                (!pressed).then(|| center(e)).transpose()?
            } else {
                Some(to_screen(f("x")?, f("y")?))
            };
            if let Some(p) = point {
                input::click(p, button, count);
                overlay::point_at(p);
            }
        }
        "drag" => {
            let (from, to) = (to_screen(f("from_x")?, f("from_y")?), to_screen(f("to_x")?, f("to_y")?));
            input::drag(from, to);
            overlay::point_at(to);
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
            ax::perform(e.0, &action.0).map_err(server_err)?;
        }
        "press_key" => input::press(a["key"].as_str().unwrap_or("")).map_err(|k| server_err(format!("keyNotFound(\"{k}\")")))?,
        "type_text" => {
            let t = a["text"].as_str().unwrap_or("");
            // Insert at the caret through AX when possible; it needs no keyboard focus.
            let focused = ax::attr(ax::app_element(app.pid).0, "AXFocusedUIElement");
            // Some apps (Chrome) accept the write but ignore it, so confirm the text changed.
            let inserted = focused.is_some_and(|f| {
                let before = ax::attr_str(f.0, "AXValue");
                ax::settable(f.0, "AXSelectedText")
                    && ax::set_attr(f.0, "AXSelectedText", ax::cfstr(t).0).is_ok()
                    && ax::attr_str(f.0, "AXValue") != before
            });
            if !inserted {
                input::type_text(t);
            }
        }
        "set_value" => {
            let e = elem()?;
            if !ax::settable(e.0, "AXValue") {
                return Err(server_err("Cannot set a value for an element that is not settable"));
            }
            ax::set_attr(e.0, "AXValue", ax::cfstr(a["value"].as_str().unwrap_or("")).0).map_err(server_err)?;
        }
        "scroll" => {
            let e = elem()?;
            let dir = a["direction"].as_str().unwrap_or("down");
            let pages = a["pages"].as_f64().unwrap_or(1.0);
            let action = format!("AXScroll{}{}ByPage", dir[..1].to_uppercase(), &dir[1..]);
            if pages.fract() == 0.0 && ax::actions(e.0).iter().any(|(n, _)| *n == action) {
                (0..pages as usize).try_for_each(|_| ax::perform(e.0, &action)).map_err(server_err)?;
            } else {
                let (_, _, w, h) = ax::frame(e.0).ok_or(server_err("element has no frame"))?;
                let px = |size: f64| (pages * size).round() as i32; // a page is the element's extent
                let (dy, dx) = match dir {
                    "up" => (-px(h), 0),
                    "down" => (px(h), 0),
                    "left" => (0, -px(w)),
                    _ => (0, px(w)),
                };
                input::scroll(center(e)?, dy, dx);
                overlay::point_at(center(e)?);
            }
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
    if let Some(menu) = ax::open_menu(root.0) {
        ax::render(menu.0, 0, usize::MAX, &mut tree);
        return Ok((tree, win));
    }
    ax::render(win.0, 0, usize::MAX, &mut tree);
    // Chromium builds its web tree lazily (first request, or after dropping it in the
    // background): give it up to 3 s to show page content, and a nearly empty window too.
    let web = apps::chromium(&app.path) && ax::enable_web(app.pid);
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
    let focused = ax::attr(root.0, "AXFocusedUIElement");
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

const BROWSER_NOTES: &str = "## Browser Computer Use\n\nWhen navigating to a new website or starting a separate web task, prefer opening a new tab instead of reusing the current tab; reuse the current tab only when the user explicitly asks to continue there or when the current page is clearly the right place to continue the existing workflow.";

/// Per-app guidance, assembled like the original: the browser note for any app that handles
/// http URLs, then the first `<name>.md` found for the app's candidate names (Music's file is
/// "AppleMusic", then CFBundleName, then the bundle id). Files come from
/// `$RUST_COMPUTER_USE_INSTRUCTIONS`, else the Codex Computer Use app's bundled AppInstructions.
fn instructions(app: &apps::App) -> String {
    let bundle = objc2_foundation::NSBundle::bundleWithPath(&objc2_foundation::NSString::from_str(&app.path));
    let info = |k: &str| bundle.as_ref().and_then(|b| b.objectForInfoDictionaryKey(&objc2_foundation::NSString::from_str(k)));
    let mut names: Vec<String> = vec![];
    if app.bundle_id == "com.apple.Music" {
        names.push("AppleMusic".into());
    }
    names.extend(info("CFBundleName").and_then(|n| n.downcast::<objc2_foundation::NSString>().ok()).map(|n| n.to_string()));
    names.push(app.bundle_id.clone());
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = [
        std::env::var("RUST_COMPUTER_USE_INSTRUCTIONS").unwrap_or_default(),
        format!("{home}/.codex/computer-use/Codex Computer Use.app/Contents/Resources/Package_ComputerUse.bundle/Contents/Resources/AppInstructions"),
    ];
    let mut parts: Vec<String> = vec![];
    if apps::handles_http(&app.path) {
        parts.push(BROWSER_NOTES.into());
    }
    let file = names.iter().find_map(|n| {
        dirs.iter().filter(|d| !d.is_empty()).find_map(|d| std::fs::read_to_string(format!("{d}/{n}.md")).ok()).filter(|t| !t.is_empty())
    });
    parts.extend(file);
    if parts.is_empty() {
        return String::new();
    }
    format!("<app_specific_instructions>\n{}\n</app_specific_instructions>\n", parts.join("\n\n"))
}

/// get_app_state passes its instructions (possibly empty) for the full framing; actions pass
/// None and get the shorter one.
fn state(snaps: &mut HashMap<i32, Snapshot>, app: &apps::App, notes: Option<String>) -> Result<Value, String> {
    let (tree, win) = render(app)?;
    let frame = ax::frame(win.0).ok_or(server_err("window has no frame"))?;
    let window_id = window_id(app.pid, frame).unwrap_or(0);
    // The window server's title first (Chrome's omits the app name), then the AX title.
    let title = window_name(window_id).or_else(|| ax::attr_str(win.0, "AXTitle")).unwrap_or_default();
    let window = format!("Window: \"{title}\", App: {}.\n{}", app.name, tree.text);
    let body = if let Some(notes) = notes {
        format!(
            "Computer Use state (CUA App Version: {CUA_VERSION})\n{notes}<app_state>\nApp={}/ (bundleID {}, pid {})\n{window}</app_state>",
            app.path, app.bundle_id, app.pid
        )
    } else {
        format!("App={} (pid {})\n{window}", app.bundle_id, app.pid)
    };
    let mut content = vec![json!({"type": "text", "text": body})];
    let mut image = (frame.2, frame.3);
    let menu_open = ax::open_menu(ax::app_element(app.pid).0).is_some(); // no screenshot then
    match screenshot(window_id) {
        Ok(_) if menu_open => {}
        Ok((jpeg, size)) => {
            image = size;
            let data = base64::engine::general_purpose::STANDARD.encode(jpeg);
            content.push(json!({"type": "image", "data": data, "mimeType": "image/jpeg"}));
        }
        Err(e) => content.push(json!({"type": "text", "text": format!("Screenshot unavailable: {e}")})),
    }
    snaps.insert(app.pid, Snapshot { tree, window: frame, window_id, image });
    Ok(Value::Array(content))
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
