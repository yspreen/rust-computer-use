//! The agent's on-screen cursor, rebuilt from the original's ComputerUseCursor: a 126x126
//! click-through window holding a dark rounded arrow with a white rim, a thin blue outer line
//! and a soft shadow. Long moves follow the
//! best of 20 candidate Bézier paths, driven by a spring; short ones "scoot" with
//! stretch, squash and tilt. All motion and drawing runs on the main thread.
use crate::input::Pt;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::*;
use objc2_foundation::{NSPoint, NSRect, NSSize, NSTimer};
use std::cell::RefCell;
use std::f64::consts::PI;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// MotionConfiguration.live and CloseEnoughConfiguration.default.
const CLICK_ANGLE: f64 = -44.0;
const BOUNDS_MARGIN: f64 = 20.0;
const START_HANDLE: f64 = 0.41960295031576633;
const END_HANDLE: f64 = 0.15;
const ARC_SIZE: f64 = 0.2765523188064277;
const ARC_FLOW: f64 = 0.5783555327868779;
const STRAIGHT_DISTANCE: f64 = 10.0;
const SCOOT_DISTANCE: f64 = 196.0;
const BLEND_START: f64 = 0.99;
const CLOSE_PROGRESS: f64 = 0.995;
const CLOSE_DISTANCE: f64 = 3.157;

/// Window size and where the click point sits in it (from the top-left).
const SIZE: f64 = 126.0;
const HOTSPOT: f64 = 42.0;
/// Each spring integrates in fixed steps, as the original's animation engine does.
const STEP: f64 = 1.0 / 240.0;
const FADE: f64 = 0.25;
/// The cursor goes away once the agent has been idle this long.
const IDLE: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, PartialEq)]
struct V(f64, f64);
impl std::ops::Add for V {
    type Output = V;
    fn add(self, o: V) -> V {
        V(self.0 + o.0, self.1 + o.1)
    }
}
impl std::ops::Sub for V {
    type Output = V;
    fn sub(self, o: V) -> V {
        V(self.0 - o.0, self.1 - o.1)
    }
}
impl std::ops::Mul<f64> for V {
    type Output = V;
    fn mul(self, k: f64) -> V {
        V(self.0 * k, self.1 * k)
    }
}
impl V {
    fn len(self) -> f64 {
        self.0.hypot(self.1)
    }
    fn dot(self, o: V) -> f64 {
        self.0 * o.0 + self.1 * o.1
    }
    fn norm(self) -> V {
        let l = self.len();
        if l < 0.001 { V(0.0, 0.0) } else { self * (1.0 / l) }
    }
    fn rot(self, a: f64) -> V {
        let (s, c) = a.sin_cos();
        V(self.0 * c - self.1 * s, self.0 * s + self.1 * c)
    }
}

/// The click direction: where the arrow's tip points at rest (AppKit y-up space).
fn click_dir() -> V {
    let t = CLICK_ANGLE.to_radians();
    V(t.sin(), t.cos())
}

/// Clockwise artwork rotation for a heading, zero when moving along the click direction.
fn heading_angle(v: V) -> f64 {
    let v = v.norm();
    if v.len() == 0.0 {
        return 0.0;
    }
    ((-v.1).atan2(v.0).to_degrees() + 90.0 - CLICK_ANGLE).to_radians()
}

fn clamp01(x: f64) -> f64 {
    x.clamp(0.0, 1.0)
}

/// A damped spring with unit mass, stepped with RK4.
#[derive(Clone, Copy)]
struct Spring {
    x: f64,
    v: f64,
    target: f64,
    k: f64,
    c: f64,
    idle: f64,
}

impl Spring {
    /// SwiftUI-style parameters: response (s) and damping fraction.
    fn new(x: f64, target: f64, response: f64, damping: f64, idle: f64) -> Spring {
        let k = (2.0 * PI / response).powi(2);
        Spring { x, v: 0.0, target, k, c: 4.0 * PI * damping / response, idle }
    }
    fn raw(x: f64, k: f64, c: f64) -> Spring {
        Spring { x, v: 0.0, target: x, k, c, idle: 0.001 }
    }
    fn step(&mut self, dt: f64) {
        let a = |x: f64, v: f64| -self.k * (x - self.target) - self.c * v;
        let (k1x, k1v) = (self.v, a(self.x, self.v));
        let (k2x, k2v) = (self.v + k1v * dt / 2.0, a(self.x + k1x * dt / 2.0, self.v + k1v * dt / 2.0));
        let (k3x, k3v) = (self.v + k2v * dt / 2.0, a(self.x + k2x * dt / 2.0, self.v + k2v * dt / 2.0));
        let (k4x, k4v) = (self.v + k3v * dt, a(self.x + k3x * dt, self.v + k3v * dt));
        self.x += dt / 6.0 * (k1x + 2.0 * k2x + 2.0 * k3x + k4x);
        self.v += dt / 6.0 * (k1v + 2.0 * k2v + 2.0 * k3v + k4v);
    }
    fn settled(&self) -> bool {
        self.v.abs() < self.idle && (self.x - self.target).abs() < 0.001
    }
}

type Cubic = [V; 4];

fn bezier(c: &Cubic, t: f64) -> V {
    let u = 1.0 - t;
    c[0] * (u * u * u) + c[1] * (3.0 * u * u * t) + c[2] * (3.0 * u * t * t) + c[3] * (t * t * t)
}

fn bezier_tangent(c: &Cubic, t: f64) -> V {
    let u = 1.0 - t;
    (c[1] - c[0]) * (3.0 * u * u) + (c[2] - c[1]) * (6.0 * u * t) + (c[3] - c[2]) * (3.0 * t * t)
}

/// How a candidate path reads: length, whether it stays on screen, and how much it turns.
struct Metrics {
    length: f64,
    in_bounds: bool,
    sum_sq: f64,
    max_turn: f64,
    total_turn: f64,
}

fn measure(path: &[Cubic], bounds: (V, V)) -> Metrics {
    let inside = |p: V| p.0 >= bounds.0 .0 && p.0 <= bounds.1 .0 && p.1 >= bounds.0 .1 && p.1 <= bounds.1 .1;
    let mut m = Metrics { length: 0.0, in_bounds: inside(path[0][0]), sum_sq: 0.0, max_turn: 0.0, total_turn: 0.0 };
    let (mut prev, mut heading): (V, Option<f64>) = (path[0][0], None);
    for c in path {
        for k in 1..=24 {
            let p = bezier(c, k as f64 / 24.0);
            let step = p - prev;
            m.length += step.len();
            m.in_bounds &= inside(p);
            if step.len() > 0.01 {
                let h = step.1.atan2(step.0);
                if let Some(last) = heading {
                    let mut turn = h - last;
                    while turn > PI {
                        turn -= 2.0 * PI;
                    }
                    while turn <= -PI {
                        turn += 2.0 * PI;
                    }
                    m.sum_sq += turn * turn;
                    m.max_turn = m.max_turn.max(turn.abs());
                    m.total_turn += turn.abs();
                }
                heading = Some(h);
            }
            prev = p;
        }
    }
    m
}

/// The original's CursorMotionPath: two direct curves and 18 two-segment arcs, all leaving
/// and arriving along the click direction; the lowest score among those on screen wins.
/// Returns the segments and the spring response for the trip.
fn plan(s: V, e: V, bounds: (V, V)) -> (Vec<Cubic>, f64) {
    let d = (e - s).len();
    let u = click_dir();
    let dir = if d < 0.001 { V(1.0, 0.0) } else { (e - s) * (1.0 / d) };
    let align = clamp01((-0.08 - dir.dot(u)) / 0.92);
    let response = |m: &Metrics, arc: bool| {
        let excess = (m.length / d.max(1.0) - 1.0).max(0.0);
        let complexity = clamp01(
            0.42 * clamp01(excess / 0.55) + 0.38 * clamp01(m.total_turn / (1.4 * PI)) + 0.2 * clamp01(m.sum_sq / 1.25),
        );
        let base = 0.28 * align + 0.22 * clamp01((m.length - 180.0) / 760.0) + 0.42 + 0.12 * complexity + if arc { 0.04 } else { 0.0 };
        (0.9 * if arc { 0.9 } else { 1.0 } * base).clamp(0.12, 2.2)
    };
    if d <= STRAIGHT_DISTANCE {
        let path = vec![[s, s + (e - s) * (1.0 / 3.0), s + (e - s) * (2.0 / 3.0), e]];
        let m = measure(&path, bounds);
        return (path, response(&m, false));
    }
    let start_len = (d * START_HANDLE).min(640.0).min(d * 0.9);
    let end_len = (d * END_HANDLE).min(640.0).min(d * 0.9);
    let (p1, p2) = (s + u * start_len, e - u * end_len);
    let t = CLICK_ANGLE.to_radians();
    let side = if t.cos() * dir.0 - t.sin() * dir.1 < 0.0 { -1.0 } else { 1.0 };
    let mid = (s + e) * 0.5;
    let arc_len = (d * ARC_SIZE).min(520.0);
    let flow_len = (d * ARC_FLOW).min(440.0);
    let shift = u * (start_len * 0.16);
    let perp = V(-dir.1, dir.0) * side;
    let mut candidates: Vec<(Vec<Cubic>, bool)> = vec![
        (vec![[s, p1, p2, e]], false),
        (vec![[s, s + u * (start_len * 0.65), e - u * (end_len * 0.65), e]], false),
    ];
    for a in [0.55, 0.80, 1.05] {
        for f in [0.65, 1.00, 1.35] {
            let flow = dir * (flow_len * f);
            for sign in [1.0, -1.0] {
                let apex = mid + shift + perp * (sign * arc_len * a);
                candidates.push((vec![[s, p1, apex - flow, apex], [apex, apex + flow, p2, e]], true));
            }
        }
    }
    let scored: Vec<(f64, bool, usize, Metrics)> = candidates
        .iter()
        .enumerate()
        .map(|(i, (path, arc))| {
            let m = measure(path, bounds);
            let excess = (m.length / d.max(1.0) - 1.0).max(0.0);
            let score = if *arc { 45.0 } else { 0.0 } + m.length + 320.0 * excess + 140.0 * m.sum_sq
                + 180.0 * m.max_turn + 18.0 * m.total_turn + 90.0 * align;
            (score, m.in_bounds, i, m)
        })
        .collect();
    let any_inside = scored.iter().any(|c| c.1);
    let best = scored.iter().filter(|c| c.1 || !any_inside).min_by(|a, b| a.0.total_cmp(&b.0)).unwrap();
    let (path, arc) = &candidates[best.2];
    (path.clone(), response(&best.3, *arc))
}

/// Point and tangent at progress p, spread evenly over the segments.
fn sample(path: &[Cubic], p: f64) -> (V, V) {
    let n = path.len() as f64;
    let (i, t) = if p >= 1.0 { (path.len() - 1, 1.0) } else { ((p * n).floor() as usize, p * n - (p * n).floor()) };
    (bezier(&path[i], t), bezier_tangent(&path[i], t))
}

struct Scoot {
    from: V,
    to: V,
    x: Spring,
    y: Spring,
    rotation: Spring,
    axis: Spring,
    squash: Spring,
    tilt: Spring,
    pivot_side: f64,
}

enum Motion {
    Still,
    Path { segments: Vec<Cubic>, progress: Spring, to: V },
    Scoot(Box<Scoot>),
}

struct Cursor {
    window: Retained<NSWindow>,
    view: Retained<CursorView>,
    timer: Option<Retained<NSTimer>>,
    /// Hotspot in AppKit global coordinates (y up).
    pos: V,
    motion: Motion,
    arrived: Option<mpsc::Sender<()>>,
    clock: Instant,
    /// Time integrated so far, in seconds since `clock`.
    simulated: f64,
    angle: Spring,
    press: Spring,
    // Scoot deformation: stretch axis (degrees), scales, pivot and tilt (radians).
    axis: f64,
    x_scale: f64,
    y_scale: f64,
    pivot: f64,
    tilt: f64,
    /// Fade: +1 appearing, -1 disappearing.
    fading: f64,
    target_window: u32,
    raised: bool,
}

thread_local! {
    static CURSOR: RefCell<Option<Cursor>> = const { RefCell::new(None) };
    /// What drawRect reads: hotspot-relative transform inputs.
    static LOOK: RefCell<Look> = const { RefCell::new(Look { angle: 0.0, scale: 1.0, axis: 0.0, x_scale: 1.0, y_scale: 1.0, pivot: 0.5, tilt: 0.0 }) };
}

#[derive(Clone, Copy)]
struct Look {
    angle: f64,
    scale: f64,
    axis: f64,
    x_scale: f64,
    y_scale: f64,
    pivot: f64,
    tilt: f64,
}

/// The arrow's outline (the original's AgentCursor shape, unit square, y down): a start
/// point, then (end, control 1, control 2) curves or line ends.
const ARROW_START: V = V(0.00599, 0.15864);
const ARROW: [(V, Option<(V, V)>); 7] = [
    (V(0.15158, 0.00627), Some((V(-0.02364, 0.06456), V(0.06169, -0.02474)))),
    (V(0.87634, 0.25652), None),
    (V(0.88794, 0.48095), Some((V(0.97594, 0.29096), V(0.9834, 0.43547)))),
    (V(0.59343, 0.62108), None),
    (V(0.45955, 0.92925), None),
    (V(0.2451, 0.91717), Some((V(0.41611, 1.02925), V(0.27801, 1.02146)))),
    (V(0.00599, 0.15864), None),
];
/// 15 pt across, the rounded tip on the click point.
const ARROW_SIZE: f64 = 15.0;
const TIP: V = V(0.03, 0.03);
/// The shadow's center, inside the arrow's body (shape points, y up).
const BODY: V = V(6.0, -6.0);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RustComputerUseCursorView"]
    struct CursorView;

    impl CursorView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            draw(LOOK.with(|l| *l.borrow()));
        }
    }
);

/// Unit outline point to shape points: tip at the origin, y up.
fn shape(q: V) -> V {
    V((q.0 - TIP.0) * ARROW_SIZE, (TIP.1 - q.1) * ARROW_SIZE)
}

/// Shape point to view coordinates (y up, origin bottom-left): press scale, heading and
/// tilt about the tip, then the scoot stretch.
fn place(look: &Look, p: V) -> V {
    let turned = (p * look.scale).rot(-(look.angle + look.tilt));
    // Scoot: stretch along the travel axis and squash across it, about the pivot end.
    let axis = look.axis.to_radians();
    let a = turned.rot(-axis);
    let half = ARROW_SIZE * look.scale / 2.0;
    let stretched = V(a.0 * look.x_scale + (1.0 - look.x_scale) * (look.pivot - 0.5) * 2.0 * half, a.1 * look.y_scale);
    stretched.rot(axis) + V(HOTSPOT, SIZE - HOTSPOT)
}

/// The "halo ring" cursor: a dark rounded arrow with a white rim and a thin blue outer line,
/// over a soft shadow.
fn draw(look: Look) {
    let pt = |v: V| NSPoint::new(v.0, v.1);
    let rgb = |r: f64, g: f64, b: f64, a: f64| NSColor::colorWithSRGBRed_green_blue_alpha(r / 255.0, g / 255.0, b / 255.0, a);
    // Shadow: a black Gaussian (sigma 10 pt, peak alpha 0.2) out to 3 sigma.
    let stops: Vec<Retained<NSColor>> = (0..=16)
        .map(|i| {
            let r = i as f64 / 16.0 * 30.0;
            rgb(0.0, 0.0, 0.0, 0.2 * (-r * r / 200.0).exp())
        })
        .collect();
    let refs: Vec<&NSColor> = stops.iter().map(|c| &**c).collect();
    if let Some(g) = NSGradient::initWithColors(NSGradient::alloc(), &objc2_foundation::NSArray::from_slice(&refs)) {
        let c = pt(place(&look, BODY));
        g.drawFromCenter_radius_toCenter_radius_options(c, 0.0, c, 30.0, NSGradientDrawingOptions::empty());
    }
    let at = |q: V| pt(place(&look, shape(q)));
    let path = NSBezierPath::bezierPath();
    path.moveToPoint(at(ARROW_START));
    for (end, controls) in ARROW {
        match controls {
            Some((c1, c2)) => path.curveToPoint_controlPoint1_controlPoint2(at(end), at(c1), at(c2)),
            None => path.lineToPoint(at(end)),
        }
    }
    path.closePath();
    path.setLineJoinStyle(NSLineJoinStyle::Round);
    path.setLineCapStyle(NSLineCapStyle::Round);
    // Blue line first and twice the rim's width, so 1.2 pt of it shows outside the rim.
    rgb(47.0, 123.0, 255.0, 1.0).setStroke();
    path.setLineWidth(4.0);
    path.stroke();
    rgb(38.0, 42.0, 52.0, 1.0).setFill();
    path.fill();
    rgb(255.0, 255.0, 255.0, 0.92).setStroke();
    path.setLineWidth(1.6);
    path.stroke();
}

fn make(mtm: MainThreadMarker) -> Cursor {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(SIZE, SIZE));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(), frame, NSWindowStyleMask::Borderless, NSBackingStoreType::Buffered, true,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setOpaque(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setHasShadow(false);
    window.setIgnoresMouseEvents(true);
    // canJoinAllApplications | ignoresCycle | transient, like the original.
    window.setCollectionBehavior(NSWindowCollectionBehavior::from_bits_retain(0x40048));
    let view: Retained<CursorView> = unsafe { msg_send![CursorView::alloc(mtm), initWithFrame: frame] };
    window.setContentView(Some(&view));
    Cursor {
        window,
        view,
        timer: None,
        pos: V(0.0, 0.0),
        motion: Motion::Still,
        arrived: None,
        clock: Instant::now(),
        simulated: 0.0,
        // The view's implicit animations: Spring(mass 1, stiffness 130.5, damping 18.85) for
        // the angle and Spring(1, 400, 33) for the press.
        angle: Spring::raw(0.0, 130.5, 18.85),
        press: Spring::raw(1.0, 400.0, 33.0),
        axis: 0.0,
        x_scale: 1.0,
        y_scale: 1.0,
        pivot: 0.5,
        tilt: 0.0,
        fading: 0.0,
        target_window: 0,
        raised: false,
    }
}

fn main_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm).firstObject().map(|s| s.frame().size.height).unwrap_or(0.0)
}

/// The visible frame (inset by the bounds margin) of the screen holding `p`, if any.
fn screen_bounds(mtm: MainThreadMarker, p: V) -> Option<(V, V)> {
    NSScreen::screens(mtm).iter().find_map(|s| {
        let f = s.frame();
        let inside = p.0 >= f.origin.x && p.0 < f.origin.x + f.size.width && p.1 >= f.origin.y && p.1 < f.origin.y + f.size.height;
        inside.then(|| {
            let v = s.visibleFrame();
            (
                V(v.origin.x + BOUNDS_MARGIN, v.origin.y + BOUNDS_MARGIN),
                V(v.origin.x + v.size.width - BOUNDS_MARGIN, v.origin.y + v.size.height - BOUNDS_MARGIN),
            )
        })
    })
}

impl Cursor {
    fn tick(&mut self) {
        let now = self.clock.elapsed().as_secs_f64();
        if now - self.simulated > 1.0 {
            self.simulated = now - 1.0 / 60.0; // fell behind (sleep, stall): resync
        }
        while self.simulated + STEP <= now {
            self.simulated += STEP;
            self.step(STEP);
        }
        self.render();
        let idle = matches!(self.motion, Motion::Still) && self.angle.settled() && self.press.settled() && self.fading == 0.0;
        if idle {
            if let Some(t) = self.timer.take() {
                t.invalidate();
            }
        }
    }

    fn step(&mut self, dt: f64) {
        self.angle.step(dt);
        self.press.step(dt);
        if self.fading != 0.0 {
            let a = (self.window.alphaValue() + self.fading * dt / FADE).clamp(0.0, 1.0);
            self.window.setAlphaValue(a);
            if a == 0.0 || a == 1.0 {
                if a == 0.0 {
                    self.window.orderOut(None);
                }
                self.fading = 0.0;
            }
        }
        match &mut self.motion {
            Motion::Still => {}
            Motion::Path { segments, progress, to } => {
                progress.step(dt);
                let p = progress.x.clamp(0.0, 1.0);
                let (pos, tangent) = sample(segments, p);
                // Over the last 1% the heading eases into the click direction.
                let w = clamp01((p - BLEND_START) / (1.0 - BLEND_START));
                let k = 1.0 - (1.0 - w).powi(3);
                let heading = (click_dir() * k + tangent.norm() * (1.0 - k)).norm();
                self.angle.target = nearest(heading_angle(heading), self.angle.x);
                self.pos = pos;
                if p >= CLOSE_PROGRESS && (pos - *to).len() <= CLOSE_DISTANCE {
                    self.arrived.take().map(|tx| tx.send(()));
                }
                if progress.settled() {
                    self.pos = *to;
                    self.finish();
                }
            }
            Motion::Scoot(sc) => {
                for s in [&mut sc.x, &mut sc.y, &mut sc.rotation, &mut sc.axis, &mut sc.squash, &mut sc.tilt] {
                    s.step(dt);
                }
                let p = V(sc.x.x, sc.y.x);
                let span = sc.to - sc.from;
                let t = if span.dot(span) < 0.001 { 1.0 } else { clamp01((p - sc.from).dot(span) / span.dot(span)) };
                let env = clamp01((t / 0.22).min(1.0) * (1.0 - t).max(0.0).powf(0.62));
                let dir = span.norm();
                sc.squash.target = 1.0 - 0.18 * env;
                sc.tilt.target = env * (dir.0 * 0.75 + dir.1 * 0.62).clamp(-1.0, 1.0) * 76.0;
                self.pos = p;
                self.x_scale = 1.0 + 0.38 * env;
                self.y_scale = sc.squash.x.clamp(0.0, 1.0);
                self.pivot = env * sc.pivot_side + (1.0 - env) * 0.5;
                self.axis = sc.axis.x;
                self.tilt = sc.tilt.x.to_radians();
                self.angle.target = sc.rotation.x.to_radians();
                self.angle.x = self.angle.target;
                if t >= CLOSE_PROGRESS && (p - sc.to).len() <= CLOSE_DISTANCE {
                    self.arrived.take().map(|tx| tx.send(()));
                }
                let speed = sc.x.v.hypot(sc.y.v);
                if speed < 12.0 && (p - sc.to).len() < 0.5 && [&sc.rotation, &sc.axis, &sc.squash, &sc.tilt].iter().all(|s| s.settled()) {
                    self.pos = sc.to;
                    (self.axis, self.x_scale, self.y_scale, self.pivot, self.tilt) = (0.0, 1.0, 1.0, 0.5, 0.0);
                    self.finish();
                }
            }
        }
    }

    fn finish(&mut self) {
        self.motion = Motion::Still;
        self.arrived.take().map(|tx| tx.send(()));
        if self.raised && top_window_at(self.cg(self.pos)) == Some(self.target_window) {
            self.stack_above_target();
        }
    }

    fn render(&self) {
        let origin = NSPoint::new(self.pos.0 - HOTSPOT, self.pos.1 - (SIZE - HOTSPOT));
        self.window.setFrameOrigin(origin);
        let press = self.press.x;
        LOOK.with(|l| {
            *l.borrow_mut() = Look {
                angle: self.angle.x,
                scale: press,
                axis: self.axis,
                x_scale: self.x_scale,
                y_scale: self.y_scale,
                pivot: self.pivot,
                tilt: self.tilt,
            }
        });
        self.view.setNeedsDisplay(true);
    }

    /// AppKit point to CG global (top-left) coordinates.
    fn cg(&self, p: V) -> Pt {
        Pt { x: p.0, y: main_height(MainThreadMarker::from(&*self.window)) - p.1 }
    }

    /// Sit just above the target window, so windows covering it cover the cursor too.
    fn stack_above_target(&mut self) {
        self.raised = false;
        self.window.setLevel(0);
        if self.target_window != 0 {
            self.window.orderWindow_relativeTo(NSWindowOrderingMode::Above, self.target_window as isize);
        } else {
            self.window.orderFrontRegardless();
        }
    }

    fn start_timer(&mut self) {
        if self.timer.is_some() {
            return;
        }
        let block = block2::RcBlock::new(|_t: std::ptr::NonNull<NSTimer>| {
            CURSOR.with(|c| c.borrow_mut().as_mut().map(Cursor::tick));
        });
        self.simulated = self.clock.elapsed().as_secs_f64();
        self.timer = Some(unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / 120.0, true, &block) });
    }

    fn move_to(&mut self, to: V, window: u32, done: mpsc::Sender<()>) {
        let mtm = MainThreadMarker::from(&*self.window);
        self.arrived = Some(done);
        self.target_window = window;
        let from = self.pos;
        let dest = self.cg(to);
        let visible = self.window.isVisible() && self.fading >= 0.0;
        let on_screen = screen_bounds(mtm, to).is_some();
        let top_at = |p: Pt| top_window_at(p) == Some(window);
        // Like the original: animate only from a visible cursor on screen, when the target
        // window is what's under the cursor or under the destination. Otherwise jump.
        let animate = visible && on_screen && (top_at(self.cg(from)) || top_at(dest));
        if !animate {
            self.pos = to;
            self.motion = Motion::Still;
            self.angle = Spring { x: 0.0, target: 0.0, v: 0.0, ..self.angle };
            self.stack_above_target();
            if !visible {
                self.window.setAlphaValue(0.0);
                self.fading = 1.0; // appear in place
            }
            self.render();
            self.arrived.take().map(|tx| tx.send(()));
            self.start_timer();
            return;
        }
        if !(top_at(self.cg(from)) && top_at(dest)) {
            // The trip crosses other windows: travel above everything, settle back on arrival.
            self.raised = true;
            self.window.setLevel(102);
            self.window.orderFrontRegardless();
        }
        if (to - from).len() <= SCOOT_DISTANCE {
            let dir = (to - from).norm();
            let (mut ang, mut side) = (dir.1.atan2(dir.0).to_degrees(), 1.0);
            if dir.len() == 0.0 {
                (ang, side) = (0.0, 0.5);
            }
            while ang > 90.0 {
                ang -= 180.0;
                side = 1.0 - side;
            }
            while ang <= -90.0 {
                ang += 180.0;
                side = 1.0 - side;
            }
            let offset = wrap180(self.angle.x.to_degrees());
            let mut sc = Scoot {
                from,
                to,
                x: Spring::new(from.0, to.0, 0.24, 0.84, 12.0),
                y: Spring::new(from.1, to.1, 0.24, 0.84, 12.0),
                rotation: Spring::new(offset, 0.0, 0.09, 0.86, 0.01),
                axis: Spring::new(self.axis, ang, 0.07, 0.82, 0.01),
                squash: Spring::new(1.0, 1.0, 0.095, 0.72, 0.01),
                tilt: Spring::new(0.0, 0.0, 0.055, 0.76, 0.01),
                pivot_side: side,
            };
            sc.rotation.x = offset;
            self.motion = Motion::Scoot(Box::new(sc));
        } else {
            let bounds = screen_bounds(mtm, to).unwrap();
            let (segments, response) = plan(from, to, bounds);
            self.motion = Motion::Path { segments, progress: Spring::new(0.0, 1.0, response, 0.9, 0.01), to };
        }
        self.start_timer();
    }
}

fn wrap180(mut d: f64) -> f64 {
    while d > 180.0 {
        d -= 360.0;
    }
    while d <= -180.0 {
        d += 360.0;
    }
    d
}

/// `a` shifted by whole turns to be closest to `current`, so the angle spring turns the
/// short way round.
fn nearest(a: f64, current: f64) -> f64 {
    a + ((current - a) / (2.0 * PI)).round() * 2.0 * PI
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> core_foundation_sys::array::CFArrayRef;
}

/// The frontmost window (of another process) containing a CG global point.
fn top_window_at(p: Pt) -> Option<u32> {
    use core_foundation_sys::array::*;
    use core_foundation_sys::dictionary::CFDictionaryGetValue;
    use core_foundation_sys::number::{kCFNumberFloat64Type, CFNumberGetValue};
    let list = crate::ax::Cf(unsafe { CGWindowListCopyWindowInfo(1 | 16, 0) } as _);
    let num = |d: *const std::ffi::c_void, k: &str| {
        let v = unsafe { CFDictionaryGetValue(d as _, crate::ax::cfstr(k).0) };
        let mut f = f64::NAN;
        if !v.is_null() {
            unsafe { CFNumberGetValue(v as _, kCFNumberFloat64Type, &mut f as *mut f64 as _) };
        }
        f
    };
    let me = std::process::id() as f64;
    (0..unsafe { CFArrayGetCount(list.0 as _) }).map(|i| unsafe { CFArrayGetValueAtIndex(list.0 as _, i) }).find_map(|d| {
        let b = unsafe { CFDictionaryGetValue(d as _, crate::ax::cfstr("kCGWindowBounds").0) };
        let (x, y, w, h) = (num(b, "X"), num(b, "Y"), num(b, "Width"), num(b, "Height"));
        let hit = p.x >= x && p.x < x + w && p.y >= y && p.y < y + h;
        let layer = num(d, "kCGWindowLayer");
        (hit && num(d, "kCGWindowOwnerPID") != me && num(d, "kCGWindowAlpha") > 0.0 && layer < 1000.0)
            .then(|| num(d, "kCGWindowNumber") as u32)
    })
}

static LAST_USE: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// Glide the cursor to `p` (CG global points) over `window`, returning once it is close
/// enough to act, as the original's click does (the spring keeps settling afterwards).
pub fn move_to(p: Pt, window: u32) {
    let (tx, rx) = mpsc::channel();
    touch();
    DispatchQueue::main().exec_async(move || {
        let mtm = MainThreadMarker::new().unwrap();
        let to = V(p.x, main_height(mtm) - p.y);
        CURSOR.with(|c| c.borrow_mut().get_or_insert_with(|| make(mtm)).move_to(to, window, tx));
    });
    let _ = rx.recv_timeout(Duration::from_secs(3));
}

/// Show the pressed state for each click: 50 ms down (cursorPressAnimationTime), 50 ms up.
pub fn press(count: i64) {
    for _ in 0..count.max(1) {
        set_pressed(true);
        std::thread::sleep(Duration::from_millis(50));
        set_pressed(false);
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn set_pressed(on: bool) {
    DispatchQueue::main().exec_async(move || {
        CURSOR.with(|c| {
            if let Some(c) = c.borrow_mut().as_mut() {
                c.press.target = if on { 0.8 } else { 1.0 };
                c.start_timer();
            }
        })
    });
}

/// Note agent activity and fade the cursor out once it has been idle for a while.
fn touch() {
    let now = Instant::now();
    *LAST_USE.lock().unwrap() = Some(now);
    std::thread::spawn(move || {
        std::thread::sleep(IDLE);
        if LAST_USE.lock().unwrap().is_some_and(|t| t <= now) {
            hide();
        }
    });
}

/// Fade out and order the window out.
pub fn hide() {
    DispatchQueue::main().exec_async(|| {
        CURSOR.with(|c| {
            if let Some(c) = c.borrow_mut().as_mut() {
                if c.window.isVisible() {
                    c.fading = -1.0;
                    c.start_timer();
                }
            }
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_pose_points_along_click_direction() {
        assert!(heading_angle(click_dir()).abs() < 1e-9);
    }

    #[test]
    fn plan_starts_and_ends_on_the_endpoints() {
        let bounds = (V(0.0, 0.0), V(2000.0, 1300.0));
        let (s, e) = (V(300.0, 300.0), V(1500.0, 900.0));
        let (path, response) = plan(s, e, bounds);
        assert_eq!(sample(&path, 0.0).0, s);
        assert!((sample(&path, 1.0).0 - e).len() < 1e-9);
        assert!((0.12..=2.2).contains(&response));
    }
}
