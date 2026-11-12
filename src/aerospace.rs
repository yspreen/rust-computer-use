//! AeroSpace, a tiling window manager, parks the windows of hidden workspaces off every display,
//! where they take no mouse events. Only used when it runs, through its documented socket
//! (guide, "Socket protocol"): a 4-byte version handshake, then length-prefixed JSON frames.
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

pub fn running() -> bool {
    !NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str("bobko.aerospace")).is_empty()
}

/// Run one AeroSpace command: whether it exits 0, and what it prints.
pub fn run(args: &[&str]) -> Option<(bool, String)> {
    let run = || -> Option<(bool, String)> {
        let user = objc2_foundation::NSUserName().to_string();
        let mut s = UnixStream::connect(format!("/tmp/bobko.aerospace-{user}.sock")).ok()?;
        s.set_read_timeout(Some(std::time::Duration::from_secs(3))).ok()?;
        s.write_all(&1u32.to_le_bytes()).ok()?;
        let mut n = [0u8; 4];
        s.read_exact(&mut n).ok()?;
        let p = json!({"args": args, "stdin": "", "windowId": null, "workspace": null}).to_string();
        s.write_all(&(p.len() as u32).to_le_bytes()).ok()?;
        s.write_all(p.as_bytes()).ok()?;
        s.read_exact(&mut n).ok()?;
        let mut b = vec![0u8; u32::from_le_bytes(n) as usize];
        s.read_exact(&mut b).ok()?;
        let answer = serde_json::from_slice::<Value>(&b).ok()?;
        Some((answer["exitCode"] == 0, answer["stdout"].as_str().unwrap_or("").to_string()))
    };
    run()
}

pub fn command(args: &[&str]) -> bool {
    run(args).is_some_and(|r| r.0)
}

/// Move a window to a workspace without following it. A config rule for new windows
/// (on-window-detected) can move it back once AeroSpace registers it, so this checks for 2 s.
/// The workspace gets accordion layout, so windows keep their size as others come and go.
pub fn park(window: u32, workspace: &str) {
    let id = window.to_string();
    let there = || run(&["list-windows", "--workspace", workspace, "--format", "%{window-id}"])
        .is_some_and(|(_, out)| out.lines().any(|l| l.trim() == id));
    let mut held = 0;
    for _ in 0..20 {
        if there() { held += 1; } else { held = 0; command(&["move-node-to-workspace", "--window-id", &id, workspace]); }
        if held == 3 { break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    command(&["layout", "--workspace", workspace, "--root", "h_accordion"]);
}

/// Turn window management off (every hidden window comes on screen) or back on (they return
/// to their workspaces).
pub fn enable(on: bool) -> bool {
    command(&["enable", if on { "on" } else { "off" }])
}
