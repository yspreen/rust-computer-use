//! Client and service halves, like the original's SkyComputerUseClient and service app.
//! macOS charges Accessibility and Screen Recording to the process responsible for the
//! caller. A server the MCP host starts directly would make the user grant them to the host
//! (a terminal, an editor, an agent app). So the binary the host starts only relays stdio to
//! the service, which LaunchServices starts from the app bundle: TCC then names this app,
//! and the setup window hands System Settings this app's bundle.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The service quits once it has had no client for this long.
const LINGER: Duration = Duration::from_secs(300);

/// The enclosing .app when this binary runs from one.
pub fn bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_path_buf())
}

/// LaunchServices starts apps as children of launchd; the MCP host starts us directly.
pub fn launched_by_launch_services() -> bool {
    std::os::unix::process::parent_id() == 1
}

fn socket() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/rust-computer-use/service.sock")
}

/// Start the app through LaunchServices, in the background, with `arg`. Always a new
/// instance: `open` would otherwise hand the arguments to one already running (setup).
pub fn launch(app: &PathBuf, arg: &str) -> bool {
    Command::new("/usr/bin/open").args(["-n", "-g", "-a"]).arg(app).args(["--args", arg]).status().is_ok_and(|s| s.success())
}

fn connect(app: &PathBuf) -> Result<UnixStream, String> {
    if let Ok(s) = UnixStream::connect(socket()) {
        return Ok(s);
    }
    if !launch(app, "--service") {
        return Err(format!("could not start {}", app.display()));
    }
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(s) = UnixStream::connect(socket()) {
            return Ok(s);
        }
    }
    Err("could not reach the service".into())
}

/// Relay stdio to the service, starting it if needed. If the service goes away (System
/// Settings quits the app after a Screen Recording grant), start it again and replay the MCP
/// handshake, so the host's session carries on. Returns when the host closes stdin.
pub fn relay(app: &PathBuf) -> Result<(), String> {
    let stream = connect(app)?;
    let writer = Arc::new(Mutex::new(stream.try_clone().map_err(|e| e.to_string())?));
    let handshake: Arc<Mutex<Vec<String>>> = Arc::default();
    let closed = Arc::new(AtomicBool::new(false));
    {
        let (writer, handshake, closed) = (writer.clone(), handshake.clone(), closed.clone());
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                let method = serde_json::from_str::<serde_json::Value>(&line).ok().and_then(|m| m["method"].as_str().map(String::from));
                if matches!(method.as_deref(), Some("initialize" | "notifications/initialized")) {
                    handshake.lock().unwrap().push(line.clone());
                }
                let _ = writeln!(writer.lock().unwrap(), "{line}");
            }
            closed.store(true, SeqCst);
            let _ = writer.lock().unwrap().shutdown(std::net::Shutdown::Write);
        });
    }
    let mut from_service = BufReader::new(stream);
    let mut out = std::io::stdout().lock();
    let mut replayed_id = None;
    loop {
        let mut line = String::new();
        if from_service.read_line(&mut line).is_ok_and(|n| n > 0) {
            // The restarted service's answer to the replayed initialize is ours, not the host's.
            let reply = serde_json::from_str::<serde_json::Value>(&line).ok();
            if replayed_id.is_some() && reply.as_ref().map(|r| r["id"].clone()) == replayed_id {
                replayed_id = None;
                continue;
            }
            // Line by line, so every message reaches the host as soon as it is complete.
            if out.write_all(line.as_bytes()).and_then(|_| out.flush()).is_err() {
                return Ok(());
            }
            continue;
        }
        if closed.load(SeqCst) {
            return Ok(());
        }
        let stream = connect(app)?;
        let mut w = writer.lock().unwrap();
        *w = stream.try_clone().map_err(|e| e.to_string())?;
        for line in handshake.lock().unwrap().iter() {
            let id = serde_json::from_str::<serde_json::Value>(line).ok().map(|m| m["id"].clone()).filter(|id| !id.is_null());
            replayed_id = id.or(replayed_id);
            let _ = writeln!(w, "{line}");
        }
        from_service = BufReader::new(stream);
    }
}

/// Serve every client that connects, each on its own thread with its own MCP session.
pub fn listen(serve: fn(Box<dyn BufRead + Send>, Box<dyn Write + Send>)) {
    let path = socket();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    // Concurrent relays may launch together. Only one service may replace the socket.
    // Keep the lock open for this listener's lifetime; the kernel releases it on exit.
    extern "C" { fn flock(fd: i32, operation: i32) -> i32; }
    let lock = std::fs::OpenOptions::new().create(true).write(true).truncate(false)
        .open(path.with_extension("lock")).expect("could not open service lock");
    if unsafe { flock(lock.as_raw_fd(), 2 | 4) } != 0 { std::process::exit(0); } // LOCK_EX | LOCK_NB
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rust-computer-use: cannot listen on {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    static CLIENTS: AtomicUsize = AtomicUsize::new(0);
    let watch = path.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(LINGER);
        if CLIENTS.load(SeqCst) == 0 {
            let _ = std::fs::remove_file(&watch);
            std::process::exit(0);
        }
    });
    for stream in listener.incoming().flatten() {
        let Ok(writer) = stream.try_clone() else { continue };
        CLIENTS.fetch_add(1, SeqCst);
        std::thread::spawn(move || {
            serve(Box::new(BufReader::new(stream)), Box::new(writer));
            CLIENTS.fetch_sub(1, SeqCst);
        });
    }
}
