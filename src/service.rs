//! Client and service halves, like the original's SkyComputerUseClient and service app.
//! macOS charges Accessibility and Screen Recording to the process responsible for the
//! caller. A server the MCP host starts directly would make the user grant them to the host
//! (a terminal, an editor, an agent app). So the binary the host starts only relays stdio to
//! the service, which LaunchServices starts from the app bundle: TCC then names this app,
//! and the setup window hands System Settings this app's bundle.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
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

/// Start the app through LaunchServices, in the background, with `arg`.
pub fn launch(app: &PathBuf, arg: &str) -> bool {
    Command::new("/usr/bin/open").arg("-g").arg("-a").arg(app).args(["--args", arg]).status().is_ok_and(|s| s.success())
}

/// Relay stdio to the service, starting it if needed. Returns when either side closes.
pub fn relay(app: &PathBuf) -> Result<(), String> {
    let mut stream = UnixStream::connect(socket());
    if stream.is_err() {
        if !launch(app, "--service") {
            return Err(format!("could not start {}", app.display()));
        }
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(50));
            stream = UnixStream::connect(socket());
            if stream.is_ok() {
                break;
            }
        }
    }
    let stream = stream.map_err(|e| format!("could not reach the service: {e}"))?;
    let mut to_service = stream.try_clone().map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut to_service);
        let _ = to_service.shutdown(std::net::Shutdown::Write);
    });
    let mut from_service = BufReader::new(stream);
    let mut out = std::io::stdout().lock();
    let mut line = String::new();
    while from_service.read_line(&mut line).is_ok_and(|n| n > 0) {
        // Line by line, so every message reaches the host as soon as it is complete.
        if out.write_all(line.as_bytes()).and_then(|_| out.flush()).is_err() {
            break;
        }
        line.clear();
    }
    Ok(())
}

/// Serve every client that connects, each on its own thread with its own MCP session.
pub fn listen(serve: fn(Box<dyn BufRead + Send>, Box<dyn Write + Send>)) {
    let path = socket();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("rust-computer-use: cannot listen on {}: {e}", path.display());
            std::process::exit(1);
        }
    };
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
