//! Browser windows a session keeps to itself (open_browser_window). Not in the original. The
//! windows live in Safari, which the user leaves to agents, so logins persist there; which
//! session owns which window is kept here, and every other session is steered away from it.
use crate::ax::{self, Cf};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

pub const BROWSER: &str = "com.apple.Safari";

/// (session, pid, window number)
static OWNED: Mutex<Vec<(u64, i32, u32)>> = Mutex::new(Vec::new());

thread_local! {
    static SESSION: Cell<u64> = const { Cell::new(0) };
}

/// Each connection serves one session on its own thread.
pub fn begin_session() {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    SESSION.with(|s| s.set(NEXT.fetch_add(1, Relaxed)));
}

fn session() -> u64 {
    SESSION.with(|s| s.get())
}

/// This session's window in the app, if it has one.
pub fn mine(pid: i32) -> Option<u32> {
    OWNED.lock().unwrap().iter().find(|o| o.0 == session() && o.1 == pid).map(|o| o.2)
}

fn others(pid: i32) -> Vec<u32> {
    OWNED.lock().unwrap().iter().filter(|o| o.0 != session() && o.1 == pid).map(|o| o.2).collect()
}

pub fn claim(pid: i32, window: u32) {
    OWNED.lock().unwrap().push((session(), pid, window));
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
            OWNED.lock().unwrap().retain(|o| !(o.0 == session() && o.2 == id));
            "Your browser window was closed. Call open_browser_window for a new one.".to_string()
        });
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

/// When the session ends its windows close (their close button, which needs no activation).
pub fn end_session() {
    let mine: Vec<(i32, u32)> = {
        let mut owned = OWNED.lock().unwrap();
        let mine = owned.iter().filter(|o| o.0 == session()).map(|o| (o.1, o.2)).collect();
        owned.retain(|o| o.0 != session());
        mine
    };
    for (pid, id) in mine {
        if let Some(close) = find(pid, id).and_then(|w| ax::attr(w.0, "AXCloseButton")) {
            let _ = ax::perform(close.0, "AXPress");
        }
    }
}
