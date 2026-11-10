//! Browser windows a session keeps to itself (open_browser_window). Not in the original. The
//! windows live in Safari, which the user leaves to agents, so logins persist there; which
//! session owns which window is kept here, and every other session is steered away from it.
use crate::ax::{self, Cf};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const BROWSER: &str = "com.apple.Safari";

/// (owner, pid, window number, last use). The owner is the session plus the name the window
/// was opened under, so subagents sharing one connection can each have a window.
type Owned = ((u64, String), i32, u32, Instant);
static OWNED: Mutex<Vec<Owned>> = Mutex::new(Vec::new());

/// A window its session hasn't used for this long closes: a client can keep a session open
/// long after its agent is done (T3 child tasks do).
const IDLE: Duration = Duration::from_secs(600);

thread_local! {
    static SESSION: Cell<u64> = const { Cell::new(0) };
    static NAME: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// The window name the current call asks for: "Safari#research" names "research".
pub fn set_name(name: &str) {
    NAME.with(|n| *n.borrow_mut() = name.to_string());
}

pub fn name() -> String {
    NAME.with(|n| n.borrow().clone())
}

fn owner() -> (u64, String) {
    (session(), name())
}

/// Each connection serves one session on its own thread.
pub fn begin_session() {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    SESSION.with(|s| s.set(NEXT.fetch_add(1, Relaxed)));
}

fn session() -> u64 {
    SESSION.with(|s| s.get())
}

/// This session's window in the app, if it has one; asking counts as using it.
pub fn mine(pid: i32) -> Option<u32> {
    let mut owned = OWNED.lock().unwrap();
    let o = owned.iter_mut().find(|o| o.0 == owner() && o.1 == pid)?;
    o.3 = Instant::now();
    Some(o.2)
}

fn others(pid: i32) -> Vec<u32> {
    OWNED.lock().unwrap().iter().filter(|o| o.0 != owner() && o.1 == pid).map(|o| o.2).collect()
}

/// The open agent windows are listed on disk, so a service that restarts (an update) closes
/// the ones it no longer knows instead of leaving them open for good.
fn record() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Application Support/RustComputerUse/agent-windows")
}

fn save(owned: &[Owned]) {
    let list: String = owned.iter().map(|o| format!("{} {}\n", o.1, o.2)).collect();
    let _ = std::fs::create_dir_all(record().parent().unwrap());
    let _ = std::fs::write(record(), list);
}

/// At service start: close the agent windows an earlier service left open.
pub fn close_leftovers() {
    let list = std::fs::read_to_string(record()).unwrap_or_default();
    for (pid, id) in list.lines().filter_map(|l| l.split_once(' ')).filter_map(|(p, w)| Some((p.parse().ok()?, w.parse().ok()?))) {
        close((pid, id));
    }
    let _ = std::fs::remove_file(record());
}

pub fn claim(pid: i32, window: u32) {
    let mut owned = OWNED.lock().unwrap();
    owned.push((owner(), pid, window, Instant::now()));
    save(&owned);
    drop(owned);
    static SWEEPER: std::sync::Once = std::sync::Once::new();
    SWEEPER.call_once(|| { std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(30));
        // The record stays, so the session's next call says its window closed instead of
        // quietly working in another one.
        let idle: Vec<(i32, u32)> = OWNED.lock().unwrap().iter().filter(|o| o.3.elapsed() > IDLE).map(|o| (o.1, o.2)).collect();
        idle.into_iter().for_each(close);
    }); });
}

/// Close a window with its close button, which needs no activation. A sheet left open (a site
/// asking for the location) disables the button: it is cancelled first. A sheet the close
/// itself brings up (leave this page?) is confirmed.
fn close((pid, id): (i32, u32)) {
    let mut pressed = false;
    for _ in 0..4 {
        let Some(w) = find(pid, id) else { return };
        let button = match ax::find_role(w.0, "AXSheet", 1) {
            Some(sheet) => ax::attr(sheet.0, if pressed { "AXDefaultButton" } else { "AXCancelButton" }),
            None => { pressed = true; ax::attr(w.0, "AXCloseButton") }
        };
        if let Some(b) = button { let _ = ax::perform(b.0, "AXPress"); }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// The app's AX window with this window number.
pub fn find(pid: i32, id: u32) -> Option<Cf> {
    let all = ax::attr(ax::app_element(pid).0, "AXWindows")?;
    ax::elements(&all).into_iter().find(|&w| ax::window_id(w) == Some(id))
        .map(|w| Cf(unsafe { core_foundation_sys::base::CFRetain(w) }))
}

/// The window a session works in: its own, else the app's focused window, unless another
/// session owns that one, then the first window nobody else owns. Ok(None): no window at all.
pub fn pick(pid: i32) -> Result<Option<Cf>, String> {
    if let Some(id) = mine(pid) {
        return find(pid, id).map(Some).ok_or_else(|| {
            let mut owned = OWNED.lock().unwrap();
            owned.retain(|o| !(o.0 == owner() && o.2 == id));
            save(&owned);
            "Your browser window was closed (by hand, or after 10 minutes unused). Call open_browser_window for a new one.".to_string()
        });
    }
    if !name().is_empty() {
        return Err(format!("There is no window named \"{}\" yet. Call open_browser_window with that name first.", name()));
    }
    let root = ax::app_element(pid);
    let taken = others(pid);
    if taken.is_empty() {
        return Ok(ax::window(root.0));
    }
    let free = |w: &Cf| ax::window_id(w.0).is_none_or(|id| !taken.contains(&id));
    let any = ax::attr(root.0, "AXWindows");
    let first_free = || any.as_ref().and_then(|all| ax::elements(all).into_iter()
        .map(|w| Cf(unsafe { core_foundation_sys::base::CFRetain(w) })).find(free));
    match ax::window(root.0).filter(free).or_else(first_free) {
        Some(w) => Ok(Some(w)),
        None if any.is_some() => Err("Every window of this app belongs to another agent's session. Call open_browser_window for one of your own.".into()),
        None => Ok(None),
    }
}

/// Make the app's first window no session owns its key window again (inside the app, without
/// activating it), so the user finds their own window there.
pub fn home(pid: i32) {
    let owned: Vec<u32> = OWNED.lock().unwrap().iter().filter(|o| o.1 == pid).map(|o| o.2).collect();
    let Some(all) = ax::attr(ax::app_element(pid).0, "AXWindows") else { return };
    if let Some(&w) = ax::elements(&all).iter().find(|&&w| ax::window_id(w).is_some_and(|id| !owned.contains(&id))) {
        let _ = ax::set_bool(w, "AXMain", true);
    }
}

/// When the session ends its windows close.
pub fn end_session() {
    let mine: Vec<(i32, u32)> = {
        let mut owned = OWNED.lock().unwrap();
        let mine = owned.iter().filter(|o| o.0 .0 == session()).map(|o| (o.1, o.2)).collect();
        owned.retain(|o| o.0 .0 != session());
        save(&owned);
        mine
    };
    mine.into_iter().for_each(close);
}
