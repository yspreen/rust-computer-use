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

/// Run one AeroSpace command; true when it exits 0.
fn command(args: &[&str]) -> bool {
    let run = || -> Option<bool> {
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
        Some(serde_json::from_slice::<Value>(&b).ok()?["exitCode"] == 0)
    };
    run().unwrap_or(false)
}

/// Turn window management off (every hidden window comes on screen) or back on (they return
/// to their workspaces).
pub fn enable(on: bool) -> bool {
    command(&["enable", if on { "on" } else { "off" }])
}
