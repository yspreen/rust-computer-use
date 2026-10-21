//! App discovery: running apps (NSWorkspace) plus recently used apps (Spotlight metadata).
use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};
use objc2_foundation::{NSBundle, NSNumber, NSString};
use std::process::Command;

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

pub fn running() -> Vec<App> {
    let ws = NSWorkspace::sharedWorkspace();
    ws.runningApplications()
        .iter()
        .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular && a.bundleIdentifier().is_some())
        .map(|a| App {
            name: s(a.localizedName()),
            path: s(a.bundleURL().and_then(|u| u.path())),
            bundle_id: s(a.bundleIdentifier()),
            pid: a.processIdentifier(),
            frontmost: a.isActive(),
        })
        .collect()
}

/// Chromium and Electron apps ship a "<Name> Framework" with Chromium's resource packs.
pub fn chromium(path: &str) -> bool {
    std::fs::read_dir(format!("{path}/Contents/Frameworks")).into_iter().flatten().flatten().any(|f| {
        f.path().join("Resources/chrome_100_percent.pak").exists()
    })
}

pub fn frontmost() -> Option<i32> {
    NSWorkspace::sharedWorkspace().frontmostApplication().map(|a| a.processIdentifier())
}

fn matches(a: &App, q: &str) -> bool {
    let q = q.trim_end_matches('/');
    a.pid.to_string() == q || a.name.eq_ignore_ascii_case(q) || a.bundle_id.eq_ignore_ascii_case(q) || a.path == q
}

/// Bundle path for an app that is not running, without launching it.
fn locate(q: &str) -> Option<String> {
    let ws = NSWorkspace::sharedWorkspace();
    if q.starts_with('/') {
        return std::path::Path::new(q).exists().then(|| q.trim_end_matches('/').to_string());
    }
    let url = ws.URLForApplicationWithBundleIdentifier(&NSString::from_str(q));
    #[allow(deprecated)]
    let path = url.and_then(|u| u.path()).or_else(|| ws.fullPathForApplication(&NSString::from_str(q)));
    path.map(|p| p.to_string())
}

/// Find a running app by name, path, or bundle id, launching it in the background if needed.
pub fn resolve(q: &str) -> Result<App, String> {
    let found = running().into_iter().find(|a| matches(a, q));
    let path = match &found {
        Some(a) => a.path.clone(),
        None => locate(q).ok_or(format!("Invalid app: {q}"))?,
    };
    if let Some(a) = found {
        return Ok(a);
    }
    let ok = Command::new("/usr/bin/open").args(["-g", "-a", &path]).status().is_ok_and(|s| s.success());
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Some(a) = running().into_iter().find(|a| ok && a.path == path) {
            return Ok(a);
        }
    }
    Err(format!("Invalid app: {q}"))
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
