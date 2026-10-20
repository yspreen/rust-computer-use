//! Permission setup, after the original's onboarding: a panel listing the two permissions, and
//! a tile docked under System Settings that the user drags into the permission list.
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::{NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}
extern "C" {
    /// The process TCC charges permissions to (exported by libsystem; used by Terminal & co).
    fn responsibility_get_pid_responsible_for_pid(pid: i32) -> i32;
}

const SETTINGS_BUNDLE: &str = "com.apple.systempreferences";

struct Permission {
    title: &'static str,
    detail: &'static str,
    pane: &'static str,
    granted: fn() -> bool,
    request: fn(),
}

const PERMISSIONS: [Permission; 2] = [
    Permission {
        title: "Accessibility",
        detail: "Reads app interfaces and operates their controls.",
        pane: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        granted: || unsafe { AXIsProcessTrusted() },
        // Prompting adds the host to the list, so often a single switch is all that is left.
        request: || unsafe {
            let k = crate::ax::cfstr("AXTrustedCheckOptionPrompt");
            let v = core_foundation_sys::number::kCFBooleanTrue;
            let opts = core_foundation_sys::dictionary::CFDictionaryCreate(
                std::ptr::null(), &(k.0 as *const _), &(v as *const _), 1,
                &core_foundation_sys::dictionary::kCFTypeDictionaryKeyCallBacks,
                &core_foundation_sys::dictionary::kCFTypeDictionaryValueCallBacks,
            );
            AXIsProcessTrustedWithOptions(opts as _);
        },
    },
    Permission {
        title: "Screen Recording",
        detail: "Takes window screenshots to find where to click.",
        pane: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        granted: || unsafe { CGPreflightScreenCaptureAccess() },
        request: || unsafe {
            CGRequestScreenCaptureAccess();
        },
    },
];

pub fn all_granted() -> bool {
    PERMISSIONS.iter().all(|p| (p.granted)())
}

/// The app that has to be allowed: macOS grants these permissions to whoever launched us
/// (a terminal, an editor, an agent app), not to this binary.
pub fn host() -> (String, Retained<NSURL>) {
    let pid = unsafe { responsibility_get_pid_responsible_for_pid(std::process::id() as i32) };
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid);
    let named = app.as_ref().and_then(|a| Some((a.localizedName()?.to_string(), a.bundleURL()?)));
    named.unwrap_or_else(|| {
        let exe = std::env::current_exe().unwrap_or_default();
        let url = NSURL::fileURLWithPath(&NSString::from_str(&exe.to_string_lossy()));
        ("rust-computer-use".into(), url)
    })
}

struct TileIvars {
    url: Retained<NSURL>,
    icon: Retained<NSImage>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements; DragTile does not implement Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustComputerUseDragTile"]
    #[ivars = TileIvars]
    struct DragTile;

    impl DragTile {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {} // keep the press so the drag starts here

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let item = NSDraggingItem::initWithPasteboardWriter(
                NSDraggingItem::alloc(),
                ProtocolObject::from_ref(&*self.ivars().url),
            );
            unsafe { item.setDraggingFrame_contents(self.bounds(), Some(&self.ivars().icon)) };
            let items = NSArray::from_retained_slice(&[item]);
            self.beginDraggingSessionWithItems_event_source(&items, event, ProtocolObject::from_ref(self));
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true // our panel never becomes active, so the first click must count
        }

        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let inside: bool = unsafe { msg_send![self, mouse: point, inRect: self.frame()] };
            if inside { self as *const Self as *mut NSView } else { std::ptr::null_mut() }
        }
    }

    unsafe impl NSObjectProtocol for DragTile {}

    unsafe impl NSDraggingSource for DragTile {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }
);

define_class!(
    // SAFETY: NSObject has no subclassing requirements; Actions does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustComputerUseSetupActions"]
    struct Actions;

    impl Actions {
        #[unsafe(method(allow:))]
        fn allow(&self, sender: &NSButton) {
            let index = sender.tag() as usize;
            let p = &PERMISSIONS[index];
            (p.request)();
            if let Some(url) = NSURL::URLWithString(&NSString::from_str(p.pane)) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
            SETUP.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.guiding = Some(index);
                    let hint = format!("Drag {} above to allow {}", s.host, p.title);
                    s.hint.setStringValue(&NSString::from_str(&hint));
                }
            });
        }
    }

    unsafe impl NSObjectProtocol for Actions {}
);

struct Setup {
    host: String,
    window: Retained<NSPanel>,
    accessory: Retained<NSPanel>,
    hint: Retained<NSTextField>,
    buttons: Vec<Retained<NSButton>>,
    timer: Retained<NSTimer>,
    _actions: Retained<Actions>,
    guiding: Option<usize>,
    exit_when_done: bool,
    /// Something was missing when the panel opened, so it closes itself once all is granted.
    pending: bool,
}

thread_local! {
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
}
static OPEN: AtomicBool = AtomicBool::new(false);

/// Open the setup panel (once at a time). With `exit_when_done` the process ends when it closes.
pub fn show(exit_when_done: bool) {
    if OPEN.swap(true, Relaxed) {
        return;
    }
    DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().unwrap();
        let setup = build(mtm, exit_when_done);
        SETUP.with(|s| *s.borrow_mut() = Some(setup));
    });
}

fn label(text: &str, size: f64, bold: bool, frame: NSRect, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    let font = if bold { NSFont::boldSystemFontOfSize(size) } else { NSFont::systemFontOfSize(size) };
    l.setFont(Some(&font));
    if !bold {
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    l.setFrame(frame);
    l
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn build(mtm: MainThreadMarker, exit_when_done: bool) -> Setup {
    let (host, host_url) = host();
    let actions: Retained<Actions> = unsafe { msg_send![Actions::alloc(mtm), init] };

    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::NonactivatingPanel;
    let window = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm), rect(0.0, 0.0, 480.0, 250.0), style, NSBackingStoreType::Buffered, false,
    );
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str("Rust Computer Use Setup"));
    window.setLevel(NSFloatingWindowLevel);
    let view = window.contentView().unwrap();
    let intro = format!("macOS grants these to {host}, the app that runs Rust Computer Use. They are only used while an agent works in your apps.");
    view.addSubview(&label("Rust Computer Use needs two permissions", 15.0, true, rect(24.0, 206.0, 432.0, 22.0), mtm));
    view.addSubview(&label(&intro, 12.0, false, rect(24.0, 160.0, 432.0, 40.0), mtm));
    let mut buttons = vec![];
    for (i, p) in PERMISSIONS.iter().enumerate() {
        let y = 104.0 - 52.0 * i as f64;
        view.addSubview(&label(p.title, 13.0, true, rect(24.0, y + 18.0, 300.0, 18.0), mtm));
        view.addSubview(&label(p.detail, 12.0, false, rect(24.0, y, 320.0, 18.0), mtm));
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Allow"), Some(&actions), Some(sel!(allow:)), mtm)
        };
        b.setTag(i as isize);
        b.setFrame(rect(364.0, y + 4.0, 92.0, 28.0));
        view.addSubview(&b);
        buttons.push(b);
    }
    let footer = format!("Screen Recording takes effect after you quit and reopen {host}.");
    view.addSubview(&label(&footer, 11.0, false, rect(24.0, 14.0, 432.0, 16.0), mtm));
    window.center();
    window.orderFrontRegardless();

    // The tile docked under System Settings: drag it into the list the pane shows.
    let accessory = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm), rect(0.0, 0.0, 420.0, 104.0),
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel, NSBackingStoreType::Buffered, false,
    );
    unsafe { accessory.setReleasedWhenClosed(false) };
    accessory.setLevel(NSFloatingWindowLevel);
    accessory.setHasShadow(true);
    accessory.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
    accessory.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::Transient);
    let pane = accessory.contentView().unwrap();
    if let Some(arrow) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str("arrow.up"), None) {
        let v = NSImageView::imageViewWithImage(&arrow, mtm);
        v.setContentTintColor(Some(&NSColor::systemBlueColor()));
        v.setFrame(rect(20.0, 66.0, 24.0, 24.0));
        pane.addSubview(&v);
    }
    let hint = label("", 13.0, true, rect(52.0, 68.0, 350.0, 20.0), mtm);
    pane.addSubview(&hint);
    let path = host_url.path().unwrap_or_default();
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
    let tile = DragTile::alloc(mtm).set_ivars(TileIvars { url: host_url, icon: icon.clone() });
    let tile: Retained<DragTile> = unsafe { msg_send![super(tile), initWithFrame: rect(20.0, 12.0, 380.0, 44.0)] };
    let frame = NSBox::new(mtm);
    frame.setBoxType(NSBoxType::Custom);
    frame.setCornerRadius(10.0);
    frame.setBorderWidth(1.0);
    frame.setBorderColor(&NSColor::separatorColor());
    frame.setFillColor(&NSColor::controlBackgroundColor());
    frame.setFrame(tile.bounds());
    tile.addSubview(&frame);
    let image = NSImageView::imageViewWithImage(&icon, mtm);
    image.setFrame(rect(10.0, 6.0, 32.0, 32.0));
    tile.addSubview(&image);
    tile.addSubview(&label(&host, 14.0, true, rect(52.0, 12.0, 320.0, 20.0), mtm));
    pane.addSubview(&tile);

    let tick = RcBlock::new(|_| SETUP.with(|s| tick(&mut s.borrow_mut())));
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.2, true, &tick) };
    Setup { host, window, accessory, hint, buttons, timer, _actions: actions, guiding: None, exit_when_done, pending: !all_granted() }
}

/// Five times a second: refresh statuses, keep the tile under System Settings, close when done.
fn tick(slot: &mut Option<Setup>) {
    let Some(s) = slot.as_mut() else { return };
    for (b, p) in s.buttons.iter().zip(&PERMISSIONS) {
        let granted = (p.granted)();
        b.setTitle(&NSString::from_str(if granted { "Granted" } else { "Allow" }));
        b.setEnabled(!granted);
    }
    if s.guiding.is_some_and(|i| (PERMISSIONS[i].granted)()) {
        s.guiding = None;
    }
    let settings = s.guiding.and_then(|_| settings_window());
    match settings {
        Some((frame, number)) => {
            s.accessory.setFrameOrigin(dock_origin(frame, s.accessory.frame().size));
            s.accessory.orderWindow_relativeTo(NSWindowOrderingMode::Above, number);
        }
        None => s.accessory.orderOut(None),
    }
    if (s.pending && all_granted()) || !s.window.isVisible() {
        s.timer.invalidate();
        s.window.close();
        s.accessory.orderOut(None);
        let exit = s.exit_when_done;
        *slot = None;
        OPEN.store(false, Relaxed);
        if exit {
            std::process::exit(0);
        }
    }
}

/// System Settings' main window (AppKit frame, window number), only while it is frontmost.
fn settings_window() -> Option<(NSRect, isize)> {
    let ws = NSWorkspace::sharedWorkspace();
    let front = ws.frontmostApplication()?;
    if front.bundleIdentifier()?.to_string() != SETTINGS_BUNDLE {
        return None;
    }
    let mtm = MainThreadMarker::new()?;
    let primary_height = NSScreen::screens(mtm).firstObject()?.frame().size.height;
    crate::windows(front.processIdentifier(), true)
        .into_iter()
        .max_by(|a, b| (a.1 .2 * a.1 .3).total_cmp(&(b.1 .2 * b.1 .3)))
        .map(|(number, (x, y, w, h))| (rect(x, primary_height - y - h, w, h), number as isize))
}

/// Centered under the pane's content (right of the sidebar), overlapping the window edge a little.
fn dock_origin(window: NSRect, size: NSSize) -> NSPoint {
    let sidebar = (window.size.width * 0.29).clamp(214.0, 272.0);
    let content_mid = window.origin.x + sidebar + (window.size.width - sidebar) / 2.0;
    NSPoint::new(content_mid - size.width / 2.0, window.origin.y - size.height + 6.0)
}
