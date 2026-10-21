//! Permission setup, after the original's onboarding: a window with a card per permission,
//! and while System Settings shows a permission list, a panel docked in its bottom-right
//! corner holding a tile of this app to drag into that list. No system prompts and no Apple
//! Events: Allow only opens the right pane.
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::{NSArray, NSAttributedString, NSDictionary, NSMutableAttributedString, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}
extern "C" {
    /// The process TCC charges permissions to (exported by libsystem; used by Terminal & co).
    fn responsibility_get_pid_responsible_for_pid(pid: i32) -> i32;
}

const SETTINGS_BUNDLE: &str = "com.apple.systempreferences";
const NAME: &str = "Rust Computer Use";

struct Permission {
    title: &'static str,
    subtitle: &'static str,
    /// Apple's own artwork for the permission, and its size in the card.
    icon: &'static str,
    icon_size: f64,
    anchor: &'static str,
    granted: fn() -> bool,
}

const PERMISSIONS: [Permission; 2] = [
    Permission {
        title: "Accessibility",
        subtitle: "Allows agents to access app interfaces",
        icon: "/System/Library/ExtensionKit/Extensions/AccessibilitySettingsExtension.appex",
        icon_size: 59.0,
        anchor: "Privacy_Accessibility",
        granted: || unsafe { AXIsProcessTrusted() },
    },
    Permission {
        title: "Screenshots",
        subtitle: "Agents use screenshots to know where to click",
        icon: "/System/Applications/Utilities/Screenshot.app",
        icon_size: 44.0,
        anchor: "Privacy_ScreenCapture",
        granted: screen_recording,
    },
];

/// A process sees a Screen Recording grant only if it started after it (System Settings
/// offers "Quit & Reopen" for that). So while this process still reads "no", ask a fresh
/// child, at most every two seconds, and remember a yes.
fn screen_recording() -> bool {
    static KNOWN: AtomicBool = AtomicBool::new(false);
    static ASKED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if KNOWN.load(Relaxed) || screen_recording_now() {
        return true;
    }
    let mut asked = ASKED.lock().unwrap();
    if asked.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(2)) {
        return false;
    }
    *asked = Some(std::time::Instant::now());
    let fresh = std::env::current_exe()
        .and_then(|exe| std::process::Command::new(exe).arg("--probe-screen").status())
        .is_ok_and(|s| s.success());
    KNOWN.store(fresh, Relaxed);
    fresh
}

pub fn screen_recording_now() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

pub fn all_granted() -> bool {
    PERMISSIONS.iter().all(|p| (p.granted)())
}

/// The app that has to be allowed: the process macOS charges our permissions to. That is
/// this app when it runs from its bundle, else whatever launched the binary.
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

/// Whether the setup window is still up, for the "still pending" answer.
pub fn is_open() -> bool {
    OPEN.load(Relaxed)
}

struct TileIvars {
    url: Retained<NSURL>,
}

/// What a view currently draws, as an image.
fn snapshot(view: &NSView) -> Retained<NSImage> {
    let image = NSImage::initWithSize(NSImage::alloc(), view.bounds().size);
    if let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(view.bounds()) {
        view.cacheDisplayInRect_toBitmapImageRep(view.bounds(), &rep);
        image.addRepresentation(&rep);
    }
    image
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
        fn mouse_down(&self, event: &NSEvent) {
            // Like the original: a press starts dragging this app's bundle URL.
            let item = NSDraggingItem::initWithPasteboardWriter(
                NSDraggingItem::alloc(),
                ProtocolObject::from_ref(&*self.ivars().url),
            );
            // Icon and name as separate pieces, as Finder drags files, so a drop target can
            // fold the row into the app's icon smoothly.
            item.setDraggingFrame(self.bounds());
            let views = self.subviews();
            let pieces: Vec<Retained<NSDraggingImageComponent>> = views
                .iter()
                .zip([unsafe { NSDraggingImageComponentIconKey }, unsafe { NSDraggingImageComponentLabelKey }])
                .map(|(view, key)| {
                    let piece = NSDraggingImageComponent::draggingImageComponentWithKey(key);
                    unsafe { piece.setContents(Some(&snapshot(&view))) };
                    piece.setFrame(view.frame());
                    piece
                })
                .collect();
            let pieces = NSArray::from_retained_slice(&pieces);
            let provider = RcBlock::new(move || std::ptr::NonNull::from(&*pieces));
            unsafe { item.setImageComponentsProvider(Some(&provider)) };
            let items = NSArray::from_retained_slice(&[item]);
            let session = self.beginDraggingSessionWithItems_event_source(&items, event, ProtocolObject::from_ref(self));
            session.setAnimatesToStartingPositionsOnCancelOrFail(true);
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true // the panel never becomes active, so the first click must count
        }

        /// The icon and name inside must not take the press: the whole row drags.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            let inside: bool = unsafe { msg_send![self, mouse: point, inRect: self.frame()] };
            if inside { self as *const Self as *mut NSView } else { std::ptr::null_mut() }
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &NSCursor::openHandCursor());
        }
    }

    unsafe impl NSObjectProtocol for DragTile {}

    unsafe impl NSDraggingSource for DragTile {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy | NSDragOperation::Generic
        }
    }
);

define_class!(
    // SAFETY: NSView has no subclassing requirements; Flipped does not implement Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustComputerUseFlippedView"]
    /// Lays out top-down, like the original's SwiftUI stacks.
    struct Flipped;

    impl Flipped {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
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
            let url = format!("x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?{}", PERMISSIONS[index].anchor);
            if let Some(url) = NSURL::URLWithString(&NSString::from_str(&url)) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
            SETUP.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.guiding = Some(index);
                    s.asked[index] = true;
                    s.hint.setAttributedStringValue(&hint(&s.host, PERMISSIONS[index].title));
                }
            });
        }

        #[unsafe(method(skip:))]
        fn skip(&self, _sender: &NSButton) {
            SETUP.with(|s| s.borrow().as_ref().map(|s| s.window.orderOut(None)));
        }

        #[unsafe(method(back:))]
        fn back(&self, _sender: &NSButton) {
            SETUP.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    s.guiding = None;
                    s.window.orderFrontRegardless();
                }
            });
        }
    }

    unsafe impl NSObjectProtocol for Actions {}
);

struct Setup {
    host: String,
    window: Retained<NSWindow>,
    accessory: Retained<NSPanel>,
    arrow: Retained<NSWindow>,
    hint: Retained<NSTextField>,
    rows: Vec<(Retained<NSButton>, Retained<NSTextField>)>,
    timer: Retained<NSTimer>,
    _actions: Retained<Actions>,
    guiding: Option<usize>,
    /// Allow was clicked for this row; it now reads "Open Settings".
    asked: [bool; 2],
    /// Screen Recording is requested only once its System Settings list is on screen, so the
    /// app is added where the user is looking.
    requested_capture: bool,
    ticks: u64,
    exit_when_done: bool,
}

thread_local! {
    static SETUP: RefCell<Option<Setup>> = const { RefCell::new(None) };
}
static OPEN: AtomicBool = AtomicBool::new(false);

/// Open the setup window (once at a time). With `exit_when_done` the process ends when it closes.
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

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn text(s: &str, font: Retained<NSFont>, primary: bool, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&NSString::from_str(s), mtm);
    l.setFont(Some(&font));
    let color = if primary { NSColor::labelColor() } else { NSColor::secondaryLabelColor() };
    l.setTextColor(Some(&color));
    l
}

fn weight(size: f64, w: f64) -> Retained<NSFont> {
    NSFont::systemFontOfSize_weight(size, w)
}

fn material(frame: NSRect, mtm: MainThreadMarker) -> Retained<NSVisualEffectView> {
    let v = NSVisualEffectView::initWithFrame(mtm.alloc(), frame);
    v.setMaterial(NSVisualEffectMaterial::Popover);
    v.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    v.setState(NSVisualEffectState::Active);
    v
}

/// "Drag <App> to the list above to allow <Permission>", the app name in the primary color.
fn hint(host: &str, permission: &str) -> Retained<NSAttributedString> {
    let out = NSMutableAttributedString::new();
    for (part, primary) in [("Drag ", false), (host, true), (" to the list above to allow ", false), (permission, true)] {
        let color = if primary { NSColor::labelColor() } else { NSColor::secondaryLabelColor() };
        let key: &NSString = unsafe { NSForegroundColorAttributeName };
        let attrs = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[&*color as &AnyObject]);
        let piece = unsafe { NSAttributedString::initWithString_attributes(NSAttributedString::alloc(), &NSString::from_str(part), Some(&attrs)) };
        out.appendAttributedString(&piece);
    }
    let font: &NSString = unsafe { NSFontAttributeName };
    unsafe { out.addAttribute_value_range(font, &NSFont::systemFontOfSize(13.0), objc2_foundation::NSRange::new(0, out.length())) };
    out.into_super()
}

/// A permission card: rounded, softly shadowed, 518 x 80.
fn card(p: &Permission, index: usize, y: f64, actions: &Actions, mtm: MainThreadMarker) -> (Retained<NSView>, Retained<NSButton>, Retained<NSTextField>) {
    let frame = rect(41.0, y, 518.0, 80.0);
    let holder = NSView::initWithFrame(mtm.alloc(), frame);
    let shadow = NSShadow::new();
    shadow.setShadowColor(Some(&NSColor::colorWithWhite_alpha(0.0, 0.12)));
    shadow.setShadowBlurRadius(12.0);
    shadow.setShadowOffset(NSSize::new(0.0, -4.0));
    holder.setShadow(Some(&shadow));
    let back = NSBox::new(mtm);
    back.setBoxType(NSBoxType::Custom);
    back.setCornerRadius(24.0);
    back.setBorderWidth(1.0);
    back.setBorderColor(&NSColor::colorWithWhite_alpha(1.0, 0.75));
    back.setFillColor(&NSColor::controlBackgroundColor());
    back.setFrame(rect(0.0, 0.0, 518.0, 80.0));
    holder.addSubview(&back);
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(p.icon));
    let image = NSImageView::imageViewWithImage(&icon, mtm);
    image.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    let s = p.icon_size;
    image.setFrame(rect(8.0 + (64.0 - s) / 2.0, (80.0 - s) / 2.0, s, s));
    holder.addSubview(&image);
    let title = text(p.title, weight(16.0, unsafe { NSFontWeightSemibold }), true, mtm);
    title.setFrame(rect(84.5, 41.0, 300.0, 21.0));
    holder.addSubview(&title);
    let sub = text(p.subtitle, NSFont::systemFontOfSize(13.0), false, mtm);
    sub.setFrame(rect(84.5, 21.0, 320.0, 18.0));
    holder.addSubview(&sub);
    let button = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("Allow"), Some(actions), Some(sel!(allow:)), mtm) };
    button.setTag(index as isize);
    button.setFrame(rect(410.0, 24.0, 88.0, 32.0));
    holder.addSubview(&button);
    let done = text("Done \u{2713}", weight(13.0, unsafe { NSFontWeightMedium }), true, mtm);
    done.setAlignment(NSTextAlignment::Right);
    done.setFrame(rect(392.0, 30.0, 100.0, 18.0));
    done.setHidden(true);
    holder.addSubview(&done);
    (holder, button, done)
}

fn build(mtm: MainThreadMarker, exit_when_done: bool) -> Setup {
    let (host, host_url) = host();
    let actions: Retained<Actions> = unsafe { msg_send![Actions::alloc(mtm), init] };

    // 600 wide; top to bottom: 28 margin, 64 icon, 20, title, description, 21, cards, 32.
    let height = 28.0 + 64.0 + 20.0 + 32.0 + 3.0 + 38.0 + 21.0 + 80.0 * 2.0 + 18.0 + 32.0;
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::FullSizeContentView;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(mtm.alloc(), rect(0.0, 0.0, 600.0, height), style, NSBackingStoreType::Buffered, false)
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(&format!("{NAME} Permissions")));
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setTitlebarAppearsTransparent(true);
    window.setMovableByWindowBackground(true);
    let root = material(rect(0.0, 0.0, 600.0, height), mtm);
    let view: Retained<Flipped> = unsafe { msg_send![Flipped::alloc(mtm), initWithFrame: rect(0.0, 0.0, 600.0, height)] };
    root.addSubview(&view);
    window.setContentView(Some(&root));

    let own = NSWorkspace::sharedWorkspace().iconForFile(&host_url.path().unwrap_or_default());
    let icon = NSImageView::imageViewWithImage(&own, mtm);
    icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    icon.setFrame(rect(268.0, 28.0, 64.0, 64.0));
    view.addSubview(&icon);
    let title = text(&format!("Enable {NAME}"), NSFont::boldSystemFontOfSize(26.0), true, mtm);
    title.setAlignment(NSTextAlignment::Center);
    title.setFrame(rect(41.0, 112.0, 518.0, 32.0));
    view.addSubview(&title);
    let description = format!(
        "{NAME} needs these permissions to use apps on your Mac.\nThese permissions are used when you ask an agent to perform tasks."
    );
    let body = text(&description, NSFont::systemFontOfSize(13.0), false, mtm);
    body.setAlignment(NSTextAlignment::Center);
    body.setFrame(rect(41.0, 147.0, 518.0, 38.0));
    view.addSubview(&body);
    let mut rows = vec![];
    for (i, p) in PERMISSIONS.iter().enumerate() {
        let (holder, button, done) = card(p, i, 206.0 + 98.0 * i as f64, &actions, mtm);
        view.addSubview(&holder);
        rows.push((button, done));
    }
    let skip = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str("Skip"), Some(&*actions), Some(sel!(skip:)), mtm) };
    skip.setBordered(false);
    skip.setFont(Some(&weight(13.0, unsafe { NSFontWeightMedium })));
    skip.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
    skip.setFrame(rect(600.0 - 57.0 - 40.0, height - 41.0, 40.0, 41.0));
    view.addSubview(&skip);
    window.center();
    window.orderFrontRegardless();

    // The panel docked in System Settings: hint, and a tile of this app to drag.
    let style = NSWindowStyleMask::Titled | NSWindowStyleMask::UtilityWindow | NSWindowStyleMask::NonactivatingPanel | NSWindowStyleMask::FullSizeContentView;
    let accessory = NSPanel::initWithContentRect_styleMask_backing_defer(NSPanel::alloc(mtm), rect(0.0, 0.0, 531.0, 110.0), style, NSBackingStoreType::Buffered, false);
    unsafe { accessory.setReleasedWhenClosed(false) };
    accessory.setLevel(NSFloatingWindowLevel);
    accessory.setMovable(false);
    accessory.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    accessory.setTitlebarAppearsTransparent(true);
    // moveToActiveSpace | transient | ignoresCycle | fullScreenNone, as the original.
    accessory.setCollectionBehavior(NSWindowCollectionBehavior::from_bits_retain(0x24a));
    let pane = material(rect(0.0, 0.0, 531.0, 110.0), mtm);
    accessory.setContentView(Some(&pane));
    let back = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(""), Some(&*actions), Some(sel!(back:)), mtm) };
    if let Some(chevron) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str("chevron.left"), Some(&NSString::from_str("Back"))) {
        back.setImage(Some(&chevron));
    }
    back.setBordered(false);
    back.setFrame(rect(18.0, 27.0, 28.0, 28.0));
    pane.addSubview(&back);
    let hint_label = NSTextField::labelWithAttributedString(&hint(&host, ""), mtm);
    hint_label.setFrame(rect(62.0, 64.0, 450.0, 20.0));
    pane.addSubview(&hint_label);
    let tile = DragTile::alloc(mtm).set_ivars(TileIvars { url: host_url });
    let tile: Retained<DragTile> = unsafe { msg_send![super(tile), initWithFrame: rect(62.0, 21.0, 451.0, 40.0)] };
    let tile_icon = NSImageView::imageViewWithImage(&own, mtm);
    tile_icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    tile_icon.setFrame(rect(0.0, 4.0, 32.0, 32.0));
    tile.addSubview(&tile_icon);
    let tile_name = text(&host, NSFont::systemFontOfSize(13.0), true, mtm);
    tile_name.setFrame(rect(36.0, 11.0, 400.0, 18.0));
    tile.addSubview(&tile_name);
    pane.addSubview(&tile);

    // The bouncing up arrow pokes out above the panel, pointing at the list.
    let arrow = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(mtm.alloc(), rect(0.0, 0.0, 100.0, 100.0), NSWindowStyleMask::Borderless, NSBackingStoreType::Buffered, false)
    };
    unsafe { arrow.setReleasedWhenClosed(false) };
    arrow.setOpaque(false);
    arrow.setBackgroundColor(Some(&NSColor::clearColor()));
    arrow.setHasShadow(false);
    arrow.setIgnoresMouseEvents(true);
    arrow.setLevel(NSFloatingWindowLevel);
    if let Some(symbol) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str("arrowshape.up.fill"), None) {
        let v = NSImageView::imageViewWithImage(&symbol, mtm);
        v.setSymbolConfiguration(Some(&NSImageSymbolConfiguration::configurationWithPointSize_weight(28.0, unsafe { NSFontWeightBold })));
        v.setContentTintColor(Some(&NSColor::systemBlueColor()));
        v.setFrame(rect(36.0, 10.0, 28.0, 34.0));
        let shadow = NSShadow::new();
        shadow.setShadowColor(Some(&NSColor::colorWithWhite_alpha(0.0, 0.23)));
        shadow.setShadowBlurRadius(7.0);
        shadow.setShadowOffset(NSSize::new(0.0, -3.0));
        v.setShadow(Some(&shadow));
        arrow.setContentView(Some(&v));
    }

    let tick = RcBlock::new(|_| SETUP.with(|s| tick(&mut s.borrow_mut())));
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.05, true, &tick) };
    Setup {
        host,
        window,
        accessory,
        arrow,
        hint: hint_label,
        rows,
        timer,
        _actions: actions,
        guiding: None,
        asked: [false; 2],
        requested_capture: false,
        ticks: 0,
        exit_when_done,
    }
}

/// Twenty times a second: refresh the cards, keep the panel docked, close when all is granted.
fn tick(slot: &mut Option<Setup>) {
    let Some(s) = slot.as_mut() else { return };
    s.ticks += 1;
    let mut first_open = true;
    for (i, ((button, done), p)) in s.rows.iter().zip(&PERMISSIONS).enumerate() {
        let granted = (p.granted)();
        button.setHidden(granted);
        done.setHidden(!granted);
        button.setTitle(&NSString::from_str(if s.asked[i] { "Open Settings" } else { "Allow" }));
        // The first card still to do answers Return.
        button.setKeyEquivalent(&NSString::from_str(if !granted && first_open { "\r" } else { "" }));
        first_open &= granted;
    }
    if s.guiding.is_some_and(|i| (PERMISSIONS[i].granted)()) {
        s.guiding = None;
    }
    let settings = s.guiding.and_then(|_| settings_window());
    match settings {
        Some((frame, number)) => {
            if s.guiding == Some(1) && !s.requested_capture && frame.size.width >= 600.0 && frame.size.height >= 470.0 {
                s.requested_capture = true;
                unsafe { CGRequestScreenCaptureAccess() };
            }
            // Inside System Settings' bottom-right corner, 10 pt in.
            let size = s.accessory.frame().size;
            let origin = NSPoint::new(frame.origin.x + frame.size.width - size.width - 10.0, frame.origin.y + 10.0);
            s.accessory.setFrameOrigin(origin);
            s.accessory.orderWindow_relativeTo(NSWindowOrderingMode::Above, number);
            let bounce = ((s.ticks as f64 * 0.05 * std::f64::consts::PI / 0.6).sin().abs()) * 8.0;
            s.arrow.setFrameOrigin(NSPoint::new(origin.x + 31.0, origin.y + 60.0 + bounce));
            s.arrow.orderWindow_relativeTo(NSWindowOrderingMode::Above, s.accessory.windowNumber());
        }
        None => {
            s.accessory.orderOut(None);
            s.arrow.orderOut(None);
        }
    }
    let skipped = !s.window.isVisible() && s.guiding.is_none();
    if all_granted() || skipped {
        s.timer.invalidate();
        s.window.close();
        s.accessory.orderOut(None);
        s.arrow.orderOut(None);
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
