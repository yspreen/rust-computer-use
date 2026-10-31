//! App discovery: running apps (NSWorkspace) plus recently used apps (Spotlight metadata).
use objc2_app_kit::{NSApplicationActivationPolicy, NSRunningApplication, NSWorkspace};
use objc2_foundation::{NSBundle, NSNumber, NSString};
use std::process::Command;

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

pub struct App {
    pub name: String,
    pub path: String,
    pub bundle_id: String,
    pub pid: i32,
    pub frontmost: bool,
}

fn s(v: Option<objc2::rc::Retained<NSString>>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

fn app(a: &NSRunningApplication) -> App {
    App {
        name: s(a.localizedName()),
        path: s(a.bundleURL().and_then(|u| u.path())),
        bundle_id: s(a.bundleIdentifier()),
        pid: a.processIdentifier(),
        frontmost: a.isActive(),
    }
}

pub fn running() -> Vec<App> {
    let ws = NSWorkspace::sharedWorkspace();
    ws.runningApplications()
        .iter()
        .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular && a.bundleIdentifier().is_some())
        // The list can lag seconds behind launches and quits; leave out apps that have quit.
        .filter(|a| unsafe { kill(a.processIdentifier(), 0) } == 0)
        .map(|a| app(&a))
        .collect()
}

/// A running instance of the bundle at `path`. LaunchServices answers this at once, when
/// NSWorkspace's list may not show an app for many seconds (Xcode, and others after it).
fn running_at(id: &str, path: &str) -> Option<App> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(id)).iter()
        .filter(|a| unsafe { kill(a.processIdentifier(), 0) } == 0)
        .map(|a| app(&a))
        .find(|a| a.path == path)
}

/// Apps whose web content needs an accessibility flag before it shows up: Chromium and
/// Electron (a "<Name> Framework" with Chromium's resource packs) and Gecko (XUL).
pub fn web_engine(path: &str) -> bool {
    std::path::Path::new(&format!("{path}/Contents/MacOS/XUL")).exists()
        || std::fs::read_dir(format!("{path}/Contents/Frameworks")).into_iter().flatten().flatten().any(|f| {
        f.path().join("Resources/chrome_100_percent.pak").exists()
    })
}

pub fn frontmost() -> Option<i32> {
    NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| a.processIdentifier())
}

fn matches(a: &App, q: &str) -> bool {
    let q = q.trim_end_matches('/');
    // The bundle's file name counts too: "Xcode-27.1.0" runs as "Xcode".
    let file = std::path::Path::new(&a.path).file_stem().and_then(|f| f.to_str()).unwrap_or("");
    a.pid.to_string() == q || a.name.eq_ignore_ascii_case(q) || a.bundle_id.eq_ignore_ascii_case(q) || a.path == q
        || file.eq_ignore_ascii_case(q)
}

/// Browsers, which get the original's browser notes and its stricter settable flags.
pub const BROWSERS: &[&str] = &[
    "com.google.Chrome", "com.apple.Safari", "com.openai.atlas", "com.openai.atlas.alpha", "com.openai.atlas.beta",
    "ai.perplexity.comet", "com.brave.Browser", "com.microsoft.edgemac", "com.operasoftware.Opera", "com.vivaldi.Vivaldi",
    "company.thebrowser.Browser", "company.thebrowser.browser", "company.thebrowser.dia", "net.imput.helium",
    "org.chromium.Chromium", "app.floorp.Floorp", "app.zen-browser.zen", "io.gitlab.librewolf-community.librewolf",
    "net.librewolf.librewolf", "net.waterfox.nightly", "net.waterfox.waterfox", "org.mozilla.firefox", "org.mozilla.nightly",
];


/// Terminals and OpenAI's own apps, which the original refuses to drive.
const BLOCKED: &[&str] = &[
    "com.apple.Terminal", "com.googlecode.iterm2", "dev.warp.Warp-Stable", "net.kovidgoyal.kitty",
    "com.github.wez.wezterm", "com.mitchellh.ghostty", "com.raphaelamorim.rio", "dev.commandline.waveterm",
    "com.openai.codex", "com.openai.codex.alpha", "com.openai.codex.beta", "com.openai.codex.dev",
    "com.openai.codex.nightly", "com.openai.chat.alpha", "com.openai.chat.beta", "com.openai.chat.nightly",
    "com.openai.chat.mac-debug",
];

/// Bundle path for an app that is not running, without launching it.
fn locate(q: &str) -> Option<String> {
    let ws = NSWorkspace::sharedWorkspace();
    if q.starts_with('/') {
        return std::path::Path::new(q).exists().then(|| q.trim_end_matches('/').to_string());
    }
    let url = ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(q));
    #[allow(deprecated)]
    // A name with a dot ("Xcode-27.1.0") only resolves with its extension spelled out.
    let path = url.and_then(|u| u.path()).or_else(|| ws.fullPathForApplication(&NSString::from_str(q)))
        .or_else(|| ws.fullPathForApplication(&NSString::from_str(&format!("{q}.app"))));
    path.map(|p| p.to_string())
}

/// Find a running app by name, path, or bundle id, launching it in the background if needed.
pub fn resolve(q: &str) -> Result<App, String> {
    let found = running().into_iter().find(|a| matches(a, q));
    let path = match &found {
        Some(a) => a.path.clone(),
        None => locate(q).ok_or(format!("Invalid app: {q}"))?,
    };
    let id = found.as_ref().map(|a| a.bundle_id.clone()).or_else(|| {
        NSBundle::bundleWithPath(&NSString::from_str(&path)).and_then(|b| b.bundleIdentifier()).map(|s| s.to_string())
    });
    if let Some(id) = id.as_ref().filter(|id| BLOCKED.contains(&id.as_str())) {
        return Err(format!("Computer Use is not allowed to use the app '{id}' for safety reasons."));
    }
    if let Some(a) = found.or_else(|| running_at(id.as_deref()?, &path)) {
        return Ok(a);
    }
    // An Electron host (Claude Code, T3 Code) leaks ELECTRON_RUN_AS_NODE, which `open` passes on
    // and which turns any Electron app into a bare node that exits at once.
    let before = NSWorkspace::sharedWorkspace().frontmostApplication();
    let ok = Command::new("/usr/bin/open")
        .args(["-g", "-a", &path])
        .env_remove("ELECTRON_RUN_AS_NODE")
        .status()
        .is_ok_and(|s| s.success());
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(a) = id.as_deref().filter(|_| ok).and_then(|id| running_at(id, &path)) {
            // The original waits for the app it launched to show a window.
            let root = crate::ax::app_element(a.pid);
            for _ in 0..50 {
                if crate::ax::window(root.0).is_some() {
                    // Electron apps activate themselves as their first window shows, whatever
                    // the launch asked for; hand the front back for as long as they keep trying.
                    let start = std::time::Instant::now();
                    while web_engine(&path) && start.elapsed() < std::time::Duration::from_secs(3) {
                        if frontmost() == Some(a.pid) {
                            #[allow(deprecated)]
                            before.as_ref().map(|b| b.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions(0)));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    return Ok(a);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            break;
        }
    }
    // Found but never came up (an Electron app that quits at once): the original times out.
    Err("Computer Use server error -10005: timeoutReached".into())
}

/// Menu-bar apps declare a boolean `LSUIElement`; the original skips them. (A string "1" does
/// not count, and `LSBackgroundOnly` is ignored, matching its listing.)
fn menu_bar_only(path: &str) -> bool {
    NSBundle::bundleWithPath(&NSString::from_str(path))
        .and_then(|b| b.objectForInfoDictionaryKey(&NSString::from_str("LSUIElement")))
        .is_some_and(|v| v.downcast_ref::<NSNumber>().is_some_and(|n| n.boolValue()))
}

/// Same Spotlight query the original service runs: apps used in the last 14 days.
/// One row per bundle id (name, path, bundle id, UTC date as the original prints it, uses).
fn recent() -> Vec<(String, String, String, String, i64)> {
    let q = r#"kMDItemContentType == "com.apple.application-bundle" && kMDItemLastUsedDate_Ranking >= $time.today(-14)"#;
    let out = Command::new("/usr/bin/mdfind").arg(q).output().map(|o| o.stdout).unwrap_or_default();
    let paths: Vec<String> = String::from_utf8_lossy(&out).lines().map(String::from).collect();
    if paths.is_empty() {
        return vec![];
    }
    let names = ["kMDItemCFBundleIdentifier", "kMDItemDisplayName", "kMDItemLastUsedDate", "kMDItemUseCount"];
    let mut cmd = Command::new("/usr/bin/mdls");
    cmd.args(["-raw", "-nullMarker", ""]);
    names.iter().for_each(|n| {
        cmd.args(["-name", n]);
    });
    let raw = cmd.args(&paths).output().map(|o| o.stdout).unwrap_or_default();
    let fields: Vec<String> = raw.split(|b| *b == 0).map(|f| String::from_utf8_lossy(f).into_owned()).collect();
    let mut rows: Vec<(String, String, String, String, i64)> = vec![];
    for (p, f) in paths.into_iter().zip(fields.chunks(names.len())) {
        if f.len() != names.len() || f[0].is_empty() || menu_bar_only(&p) {
            continue;
        }
        let row = (f[1].trim_end_matches(".app").to_string(), p, f[0].clone(), f[2].get(..10).unwrap_or("").to_string(), f[3].parse().unwrap_or(0));
        match rows.iter_mut().find(|r| r.2 == row.2) {
            Some(r) if (&r.3, r.4) < (&row.3, row.4) => *r = row,
            Some(_) => {}
            None => rows.push(row),
        }
    }
    rows
}

pub fn list() -> String {
    let recent = recent();
    let usage = |bid: &str| recent.iter().find(|r| r.2 == bid).map(|r| (r.3.clone(), r.4));
    let mut rows: Vec<(bool, bool, String, i64, String)> = vec![];
    let run = running();
    for a in &run {
        let (date, uses) = usage(&a.bundle_id).unwrap_or_default();
        let mut tags = vec![];
        if a.frontmost {
            tags.push("frontmost".to_string());
        }
        tags.push("running".into());
        if !date.is_empty() {
            tags.push(format!("last-used={date}"));
            tags.push(format!("uses={uses}"));
        }
        // Running apps print their bundle URL path, which ends in a slash.
        let line = format!("{} — {}/ — {} [{}]", a.name, a.path, a.bundle_id, tags.join(", "));
        rows.push((a.frontmost, true, date, uses, line));
    }
    for (name, path, bid, date, uses) in &recent {
        // A running app whose bundle id extends this one counts as the same app
        // (com.apple.Safari.WebApp.<id> hides com.apple.Safari), as in the original.
        if !run.iter().any(|a| a.bundle_id.starts_with(bid.as_str()) || &a.path == path) {
            let line = format!("{name} — {path} — {bid} [last-used={date}, uses={uses}]");
            rows.push((false, false, date.clone(), *uses, line));
        }
    }
    rows.sort_by(|a, b| (b.0, b.1, &b.2, b.3).cmp(&(a.0, a.1, &a.2, a.3)));
    rows.into_iter().map(|r| r.4).collect::<Vec<_>>().join("\n")
}
