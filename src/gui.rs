//! The `gui` module: windows with widgets and a drawing canvas, built on egui
//! (immediate mode: your function draws the whole UI every frame and widgets
//! return what the user did).
//!
//! ```faston
//! import gui;
//! let name = "";
//! def draw(ui) {
//!     global name;
//!     ui.heading("Hello");
//!     name = ui.textbox(name, hint="your name");
//!     if (ui.button("Greet")) { print("hi", name); }
//! }
//! gui.run(draw, title="Demo");
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use eframe::egui;
use egui::{Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use crate::value::*;
use crate::vm::Vm;

type R = Result<Value, String>;

/// A `ui` value: points at an egui `Ui` that only lives during the callback it was
/// passed to (the pointer is cleared when the callback returns).
struct UiRef {
    ui: *mut egui::Ui,
}

/// A canvas: a painter over a rectangle, valid during the frame it was made in.
struct CanvasRef {
    painter: egui::Painter,
    rect: Rect,
    hover: Option<Pos2>,
    clicked: bool,
    down: bool,
    frame: u64,
}

#[derive(Default)]
struct GuiState {
    textures: HashMap<String, egui::TextureHandle>,
    /// First error raised by a callback; the window closes and gui.run raises it.
    error: Option<String>,
    frame: u64,
    ran: bool,
}

thread_local! {
    static STATE: RefCell<GuiState> = RefCell::new(GuiState::default());
}

static UI_TYPE: HandleType = HandleType { name: "gui.Ui", method: ui_method, attr: no_attr, repr: |_| "<gui.Ui>".into() };
static CANVAS_TYPE: HandleType = HandleType {
    name: "gui.Canvas",
    method: canvas_method,
    attr: canvas_attr,
    repr: |_| "<gui.Canvas>".into(),
};

fn no_attr(_: &mut Vm, _: Gc, _: &str) -> Option<R> {
    None
}

pub fn register(vm: &mut Vm) {
    let run = Value::Obj(vm.heap.alloc(Obj::Native(Native { name: "run", func: run_native, kwargs: sym::ANY_KW })));
    let rgb = vm.native("rgb", rgb_native);
    vm.define_module("gui", vec![("run", run), ("rgb", rgb)]);
}

// ---------------------------------------------------------------- helpers

fn kw(vm: &mut Vm, name: &str) -> Option<Value> {
    let s = vm.syms.intern(name);
    vm.take_kw(s)
}

fn num(v: Value, what: &str) -> Result<f32, String> {
    match v {
        Value::Int(i) => Ok(i as f32),
        Value::Float(f) => Ok(f as f32),
        Value::Bool(b) => Ok(b as i64 as f32),
        _ => Err(format!("TypeError: {} must be a number, not '{}'", what, v.type_name())),
    }
}

fn opt_num(v: Option<Value>, what: &str) -> Result<Option<f32>, String> {
    match v {
        None | Some(Value::None) => Ok(None),
        Some(v) => num(v, what).map(Some),
    }
}

fn text_of(vm: &mut Vm, v: Value) -> Result<String, String> {
    vm.str_of(v)
}

fn arg(args: &[Value], i: usize, method: &str) -> Result<Value, String> {
    args.get(i).copied().ok_or_else(|| format!("TypeError: {}() is missing argument {}", method, i + 1))
}

fn truthy_kw(vm: &mut Vm, name: &str, default: bool) -> Result<bool, String> {
    match kw(vm, name) {
        Some(v) => vm.truthy(v),
        None => Ok(default),
    }
}

const NAMED_COLORS: &[(&str, [u8; 4])] = &[
    ("white", [255, 255, 255, 255]),
    ("black", [0, 0, 0, 255]),
    ("red", [230, 41, 55, 255]),
    ("green", [0, 200, 80, 255]),
    ("blue", [0, 121, 241, 255]),
    ("yellow", [253, 249, 0, 255]),
    ("orange", [255, 161, 0, 255]),
    ("purple", [160, 60, 200, 255]),
    ("pink", [255, 109, 194, 255]),
    ("brown", [127, 106, 79, 255]),
    ("gray", [130, 130, 130, 255]),
    ("grey", [130, 130, 130, 255]),
    ("lightgray", [200, 200, 200, 255]),
    ("darkgray", [80, 80, 80, 255]),
    ("cyan", [0, 220, 220, 255]),
    ("magenta", [255, 0, 255, 255]),
    ("gold", [255, 203, 0, 255]),
    ("lime", [0, 158, 47, 255]),
    ("teal", [0, 128, 128, 255]),
    ("navy", [0, 0, 128, 255]),
    ("transparent", [0, 0, 0, 0]),
];

/// "red", "#f80", "#ff8800", "#ff880080", (r, g, b) or (r, g, b, a).
fn color(vm: &mut Vm, v: Value) -> Result<Color32, String> {
    if let Some(s) = v.as_str() {
        let s = s.trim();
        if let Some(hex) = s.strip_prefix('#') {
            let digits: Vec<u8> = hex
                .chars()
                .map(|c| c.to_digit(16).map(|d| d as u8))
                .collect::<Option<Vec<u8>>>()
                .ok_or_else(|| format!("ValueError: invalid color '{}'", s))?;
            let rgba = match digits.len() {
                3 => [digits[0] * 17, digits[1] * 17, digits[2] * 17, 255],
                4 => [digits[0] * 17, digits[1] * 17, digits[2] * 17, digits[3] * 17],
                6 | 8 => {
                    let b = |i: usize| digits[i] * 16 + digits[i + 1];
                    [b(0), b(2), b(4), if digits.len() == 8 { b(6) } else { 255 }]
                }
                _ => return Err(format!("ValueError: invalid color '{}'", s)),
            };
            return Ok(Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]));
        }
        let lower = s.to_ascii_lowercase();
        return NAMED_COLORS
            .iter()
            .find(|(n, _)| *n == lower)
            .map(|(_, c)| Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
            .ok_or_else(|| format!("ValueError: unknown color '{}' (use a name like \"red\", \"#rrggbb\" or (r, g, b))", s));
    }
    let parts = vm.to_vec(v).map_err(|_| format!("TypeError: a color must be a string or (r, g, b), not '{}'", v.type_name()))?;
    let c = |i: usize| -> Result<u8, String> {
        match parts.get(i) {
            Some(Value::Int(n)) => Ok((*n).clamp(0, 255) as u8),
            Some(Value::Float(f)) => Ok((f * 255.0).clamp(0.0, 255.0) as u8),
            _ => Err("TypeError: color components must be numbers".into()),
        }
    };
    match parts.len() {
        3 => Ok(Color32::from_rgb(c(0)?, c(1)?, c(2)?)),
        4 => Ok(Color32::from_rgba_unmultiplied(c(0)?, c(1)?, c(2)?, c(3)?)),
        _ => Err("ValueError: a color tuple needs 3 or 4 components".into()),
    }
}

fn color_kw(vm: &mut Vm, name: &str, default: Color32) -> Result<Color32, String> {
    match kw(vm, name) {
        None | Some(Value::None) => Ok(default),
        Some(v) => color(vm, v),
    }
}

fn color_text(c: Color32) -> String {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    if a == 255 {
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", r, g, b, a)
    }
}

/// gui.rgb(r, g, b[, a]) -> "#rrggbb"
fn rgb_native(vm: &mut Vm, args: &[Value]) -> R {
    if args.len() < 3 || args.len() > 4 {
        return Err("TypeError: rgb() takes 3 or 4 arguments".into());
    }
    let t = vm.heap.tuple(args.to_vec());
    let c = color(vm, t)?;
    Ok(vm.heap.string(color_text(c)))
}

fn key_of(name: &str) -> Option<egui::Key> {
    let lower = name.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "esc" | "escape" => "Escape",
        "enter" | "return" => "Enter",
        "space" => "Space",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" | "del" => "Delete",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "home" => "Home",
        "end" => "End",
        "insert" => "Insert",
        _ => "",
    };
    if !alias.is_empty() {
        return egui::Key::from_name(alias);
    }
    let mut cap = lower.clone();
    if let Some(f) = cap.get_mut(0..1) {
        f.make_ascii_uppercase();
    }
    egui::Key::from_name(name)
        .or_else(|| egui::Key::from_name(&cap))
        .or_else(|| egui::Key::from_name(&lower.to_ascii_uppercase()))
        .or_else(|| egui::Key::from_name(&format!("Num{}", lower)))
}

/// key_down / key_pressed for a key or modifier name ("shift", "ctrl", "alt").
fn key_state(ctx: &egui::Context, name: &str, pressed: bool) -> Result<bool, String> {
    let lower = name.to_ascii_lowercase();
    if !pressed {
        let m = ctx.input(|i| i.modifiers);
        match lower.as_str() {
            "shift" => return Ok(m.shift),
            "ctrl" | "control" => return Ok(m.ctrl),
            "alt" => return Ok(m.alt),
            "cmd" | "command" | "super" => return Ok(m.command),
            _ => {}
        }
    }
    let key = key_of(name).ok_or_else(|| format!("ValueError: unknown key '{}'", name))?;
    Ok(ctx.input(|i| if pressed { i.key_pressed(key) } else { i.key_down(key) }))
}

fn texture(ctx: &egui::Context, path: &str) -> Result<egui::TextureHandle, String> {
    if let Some(t) = STATE.with(|s| s.borrow().textures.get(path).cloned()) {
        return Ok(t);
    }
    let img = image::open(path).map_err(|e| match e {
        image::ImageError::IoError(io) => crate::fileio::os_error(&io, path),
        other => format!("ValueError: can't load image '{}': {}", path, other),
    })?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let ci = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    let tex = ctx.load_texture(path, ci, egui::TextureOptions::LINEAR);
    STATE.with(|s| s.borrow_mut().textures.insert(path.to_string(), tex.clone()));
    Ok(tex)
}

fn new_ui(vm: &mut Vm, ui: &mut egui::Ui) -> Value {
    let h = Handle { ty: &UI_TYPE, data: Box::new(UiRef { ui: ui as *mut egui::Ui }) };
    Value::Obj(vm.heap.alloc(Obj::Handle(Box::new(h))))
}

fn invalidate(h: Value) {
    if let Value::Obj(g) = h {
        if let Obj::Handle(hd) = g.get_mut() {
            if let Some(u) = hd.get_mut::<UiRef>() {
                u.ui = std::ptr::null_mut();
            }
        }
    }
}

/// Calls Faston function `f` with a `ui` for `ui`.
fn call_with_ui(vm: &mut Vm, f: Value, ui: &mut egui::Ui) -> Result<(), String> {
    let h = new_ui(vm, ui);
    let r = vm.call(f, &[h]);
    invalidate(h);
    r.map(|_| ())
}

/// Same, for egui's `&mut [Ui]` (columns): the callback gets a list of uis.
fn call_with_uis(vm: &mut Vm, f: Value, uis: &mut [egui::Ui]) -> Result<(), String> {
    let hs: Vec<Value> = uis.iter_mut().map(|u| new_ui(vm, u)).collect();
    let list = vm.heap.list(hs.clone());
    let r = vm.call(f, &[list]);
    for h in hs {
        invalidate(h);
    }
    r.map(|_| ())
}

fn ui_of<'a>(g: Gc) -> Result<&'a mut egui::Ui, String> {
    let p = match g.get() {
        Obj::Handle(h) => h.get::<UiRef>().map(|u| u.ui),
        _ => None,
    };
    match p {
        Some(p) if !p.is_null() => Ok(unsafe { &mut *p }),
        _ => Err("RuntimeError: this ui is only usable inside the function it was passed to".into()),
    }
}

// ---------------------------------------------------------------- gui.run

struct App {
    vm: *mut Vm,
    update: Value,
    panels: Vec<(char, Value, Option<f32>)>,
    animate: bool,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let vm = unsafe { &mut *self.vm };
        let ctx = ui.ctx().clone();
        STATE.with(|s| s.borrow_mut().frame += 1);
        let failed = |e: String| {
            STATE.with(|s| {
                let mut s = s.borrow_mut();
                if s.error.is_none() {
                    s.error = Some(e);
                }
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        };
        if STATE.with(|s| s.borrow().error.is_some()) {
            return;
        }
        for &(side, f, size) in &self.panels {
            let id = format!("faston-panel-{}", side);
            let mut panel = match side {
                'l' => egui::Panel::left(id),
                'r' => egui::Panel::right(id),
                't' => egui::Panel::top(id),
                _ => egui::Panel::bottom(id),
            };
            if let Some(s) = size {
                panel = panel.default_size(s);
            }
            let mut r = Ok(());
            panel.show(ui, |pui| r = call_with_ui(vm, f, pui));
            if let Err(e) = r {
                return failed(e);
            }
        }
        let mut r = Ok(());
        egui::CentralPanel::default().show(ui, |cui| r = call_with_ui(vm, self.update, cui));
        if let Err(e) = r {
            return failed(e);
        }
        if self.animate {
            ctx.request_repaint();
        }
    }
}

fn run_native(vm: &mut Vm, args: &[Value]) -> R {
    if cfg!(target_os = "macos") {
        return Err("RuntimeError: the gui module isn't supported on macOS yet (its window must run on the main thread)".into());
    }
    if args.len() != 1 {
        return Err(format!("TypeError: run() takes 1 positional argument (the draw function), {} given", args.len()));
    }
    let update = args[0];
    let title = match kw(vm, "title") {
        Some(v) => text_of(vm, v)?,
        None => "Faston".to_string(),
    };
    let width = opt_num(kw(vm, "width"), "width")?.unwrap_or(800.0);
    let height = opt_num(kw(vm, "height"), "height")?.unwrap_or(600.0);
    let animate = truthy_kw(vm, "animate", false)?;
    let resizable = truthy_kw(vm, "resizable", true)?;
    let dark = match kw(vm, "dark") {
        None | Some(Value::None) => None,
        Some(v) => Some(vm.truthy(v)?),
    };
    let panel_size = opt_num(kw(vm, "panel_size"), "panel_size")?;
    let mut panels = Vec::new();
    for (name, side) in [("top", 't'), ("bottom", 'b'), ("left", 'l'), ("right", 'r')] {
        if let Some(f) = kw(vm, name) {
            if !matches!(f, Value::None) {
                panels.push((side, f, panel_size));
            }
        }
    }
    if STATE.with(|s| s.borrow().ran) {
        return Err("RuntimeError: gui.run() can only be called once per program (use ui.window() for more windows)".into());
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.ran = true;
        s.error = None;
    });
    vm.flush_all();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title.clone())
            .with_inner_size([width, height])
            .with_resizable(resizable),
        event_loop_builder: Some(Box::new(|b| {
            // Faston programs run on their own thread (for a big stack).
            #[cfg(windows)]
            {
                use winit::platform::windows::EventLoopBuilderExtWindows;
                b.with_any_thread(true);
            }
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                use winit::platform::x11::EventLoopBuilderExtX11;
                b.with_any_thread(true);
            }
            let _ = b;
        })),
        ..Default::default()
    };
    let app = App { vm: vm as *mut Vm, update, panels, animate };
    let r = eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            match dark {
                Some(true) => cc.egui_ctx.set_visuals(egui::Visuals::dark()),
                Some(false) => cc.egui_ctx.set_visuals(egui::Visuals::light()),
                None => {}
            }
            Ok(Box::new(app))
        }),
    );
    STATE.with(|s| s.borrow_mut().textures.clear());
    if let Some(e) = STATE.with(|s| s.borrow_mut().error.take()) {
        return Err(e);
    }
    r.map_err(|e| format!("RuntimeError: can't open a window: {}", e))?;
    Ok(Value::None)
}

// ---------------------------------------------------------------- ui methods

const UI_METHODS: &[&str] = &[
    "label", "text", "heading", "monospace", "hyperlink", "button", "checkbox", "radio", "selectable",
    "slider", "drag", "textbox", "textarea", "combo", "color", "progress", "spinner", "separator",
    "space", "image", "horizontal", "vertical", "group", "indent", "scroll", "collapsing", "columns",
    "window", "canvas", "key_down", "key_pressed", "mouse", "mouse_down", "clicked", "dt", "time",
    "fps", "width", "height", "close", "repaint", "set_dark",
];

fn ui_method(vm: &mut Vm, g: Gc, name: &str, args: &[Value]) -> Option<R> {
    if !UI_METHODS.contains(&name) {
        return None;
    }
    Some((|| -> R {
        let ui = ui_of(g)?;
        let nargs = |min: usize, max: usize| -> Result<(), String> {
            if args.len() < min || args.len() > max {
                Err(format!("TypeError: ui.{}() takes {} to {} arguments ({} given)", name, min, max, args.len()))
            } else {
                Ok(())
            }
        };
        match name {
            "label" | "text" | "heading" | "monospace" => {
                nargs(1, 1)?;
                let t = text_of(vm, args[0])?;
                let mut rt = RichText::new(t);
                if name == "heading" {
                    rt = rt.heading();
                }
                if name == "monospace" {
                    rt = rt.monospace();
                }
                if let Some(s) = opt_num(kw(vm, "size"), "size")? {
                    rt = rt.size(s);
                }
                if let Some(c) = kw(vm, "color") {
                    if !matches!(c, Value::None) {
                        rt = rt.color(color(vm, c)?);
                    }
                }
                if truthy_kw(vm, "bold", false)? {
                    rt = rt.strong();
                }
                if truthy_kw(vm, "italic", false)? {
                    rt = rt.italics();
                }
                ui.label(rt);
                Ok(Value::None)
            }
            "hyperlink" => {
                nargs(1, 2)?;
                let url = text_of(vm, args[0])?;
                let label = match args.get(1) {
                    Some(v) => text_of(vm, *v)?,
                    None => url.clone(),
                };
                ui.hyperlink_to(label, url);
                Ok(Value::None)
            }
            "button" => {
                nargs(1, 1)?;
                let t = text_of(vm, args[0])?;
                let enabled = truthy_kw(vm, "enabled", true)?;
                let r = ui.add_enabled(enabled, egui::Button::new(t));
                Ok(Value::Bool(r.clicked()))
            }
            "checkbox" => {
                nargs(2, 2)?;
                let t = text_of(vm, args[0])?;
                let mut v = vm.truthy(args[1])?;
                ui.checkbox(&mut v, t);
                Ok(Value::Bool(v))
            }
            "radio" | "selectable" => {
                nargs(2, 2)?;
                let t = text_of(vm, args[0])?;
                let sel = vm.truthy(args[1])?;
                let r = if name == "radio" { ui.radio(sel, t) } else { ui.selectable_label(sel, t) };
                Ok(Value::Bool(r.clicked()))
            }
            "slider" => {
                nargs(4, 4)?;
                let t = text_of(vm, args[0])?;
                let ints = matches!((args[1], args[2], args[3]), (Value::Int(_), Value::Int(_), Value::Int(_)));
                if ints {
                    let (mut v, lo, hi) = match (args[1], args[2], args[3]) {
                        (Value::Int(a), Value::Int(b), Value::Int(c)) => (a, b, c),
                        _ => unreachable!(),
                    };
                    ui.add(egui::Slider::new(&mut v, lo..=hi).text(t));
                    Ok(Value::Int(v))
                } else {
                    let mut v = num(args[1], "value")? as f64;
                    let (lo, hi) = (num(args[2], "min")? as f64, num(args[3], "max")? as f64);
                    let mut s = egui::Slider::new(&mut v, lo..=hi).text(t);
                    if let Some(step) = opt_num(kw(vm, "step"), "step")? {
                        s = s.step_by(step as f64);
                    }
                    ui.add(s);
                    Ok(Value::Float(v))
                }
            }
            "drag" => {
                nargs(2, 2)?;
                let t = text_of(vm, args[0])?;
                let speed = opt_num(kw(vm, "speed"), "speed")?.unwrap_or(1.0) as f64;
                let lo = opt_num(kw(vm, "min"), "min")?.map(|x| x as f64);
                let hi = opt_num(kw(vm, "max"), "max")?.map(|x| x as f64);
                let range = lo.unwrap_or(f64::NEG_INFINITY)..=hi.unwrap_or(f64::INFINITY);
                let r = if let Value::Int(mut v) = args[1] {
                    ui.horizontal(|ui| {
                        ui.label(t);
                        let (a, b) = (*range.start(), *range.end());
                        ui.add(egui::DragValue::new(&mut v).speed(speed).range(a.max(i64::MIN as f64) as i64..=b.min(i64::MAX as f64) as i64));
                    });
                    Value::Int(v)
                } else {
                    let mut v = num(args[1], "value")? as f64;
                    ui.horizontal(|ui| {
                        ui.label(t);
                        ui.add(egui::DragValue::new(&mut v).speed(speed).range(range));
                    });
                    Value::Float(v)
                };
                Ok(r)
            }
            "textbox" | "textarea" => {
                nargs(1, 1)?;
                let mut s = text_of(vm, args[0])?;
                let hint = match kw(vm, "hint") {
                    Some(v) => text_of(vm, v)?,
                    None => String::new(),
                };
                let width = opt_num(kw(vm, "width"), "width")?;
                let mut te = if name == "textbox" {
                    egui::TextEdit::singleline(&mut s).password(truthy_kw(vm, "password", false)?)
                } else {
                    let rows = opt_num(kw(vm, "rows"), "rows")?.unwrap_or(4.0);
                    egui::TextEdit::multiline(&mut s).desired_rows(rows as usize)
                };
                te = te.hint_text(hint);
                if let Some(w) = width {
                    te = te.desired_width(w);
                }
                ui.add(te);
                Ok(vm.heap.string(s))
            }
            "combo" => {
                nargs(3, 3)?;
                let label = text_of(vm, args[0])?;
                let options = vm.to_vec(args[1])?;
                let mut texts = Vec::with_capacity(options.len());
                let mut idx = None;
                for (i, o) in options.iter().enumerate() {
                    texts.push(text_of(vm, *o)?);
                    if idx.is_none() && crate::ops::equals(*o, args[2]) {
                        idx = Some(i);
                    }
                }
                let mut sel = idx.unwrap_or(usize::MAX);
                let shown = texts.get(sel).cloned().unwrap_or_default();
                egui::ComboBox::from_label(label).selected_text(shown).show_ui(ui, |ui| {
                    for (i, t) in texts.iter().enumerate() {
                        ui.selectable_value(&mut sel, i, t.as_str());
                    }
                });
                Ok(options.get(sel).copied().unwrap_or(args[2]))
            }
            "color" => {
                nargs(1, 1)?;
                let mut c = color(vm, args[0])?;
                ui.color_edit_button_srgba(&mut c);
                Ok(vm.heap.string(color_text(c)))
            }
            "progress" => {
                nargs(1, 1)?;
                let f = num(args[0], "fraction")?.clamp(0.0, 1.0);
                let mut pb = egui::ProgressBar::new(f);
                match kw(vm, "text") {
                    Some(Value::None) | None => pb = pb.show_percentage(),
                    Some(t) => pb = pb.text(text_of(vm, t)?),
                }
                ui.add(pb);
                Ok(Value::None)
            }
            "spinner" => {
                nargs(0, 0)?;
                ui.spinner();
                Ok(Value::None)
            }
            "separator" => {
                nargs(0, 0)?;
                ui.separator();
                Ok(Value::None)
            }
            "space" => {
                nargs(0, 1)?;
                let px = match args.first() {
                    Some(v) => num(*v, "space")?,
                    None => 8.0,
                };
                ui.add_space(px);
                Ok(Value::None)
            }
            "image" => {
                nargs(1, 1)?;
                let path = text_of(vm, args[0])?;
                let tex = texture(ui.ctx(), &path)?;
                let [tw, th] = tex.size();
                let w = opt_num(kw(vm, "width"), "width")?;
                let h = opt_num(kw(vm, "height"), "height")?;
                let (w, h) = match (w, h) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => (w, w * th as f32 / tw as f32),
                    (None, Some(h)) => (h * tw as f32 / th as f32, h),
                    (None, None) => (tw as f32, th as f32),
                };
                ui.image(egui::load::SizedTexture::new(tex.id(), Vec2::new(w, h)));
                Ok(Value::None)
            }
            "horizontal" | "vertical" | "group" | "indent" | "scroll" => {
                nargs(1, 1)?;
                let f = args[0];
                let mut r = Ok(());
                match name {
                    "horizontal" => {
                        ui.horizontal(|ui| r = call_with_ui(vm, f, ui));
                    }
                    "vertical" => {
                        ui.vertical(|ui| r = call_with_ui(vm, f, ui));
                    }
                    "group" => {
                        ui.group(|ui| r = call_with_ui(vm, f, ui));
                    }
                    "indent" => {
                        let id = ui.next_auto_id();
                        ui.indent(id, |ui| r = call_with_ui(vm, f, ui));
                    }
                    _ => {
                        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, true]);
                        if let Some(h) = opt_num(kw(vm, "height"), "height")? {
                            sa = sa.max_height(h);
                        }
                        let id = ui.next_auto_id();
                        sa.id_salt(id).show(ui, |ui| r = call_with_ui(vm, f, ui));
                    }
                }
                r.map(|_| Value::None)
            }
            "collapsing" => {
                nargs(2, 2)?;
                let title = text_of(vm, args[0])?;
                let f = args[1];
                let open = truthy_kw(vm, "open", false)?;
                let mut r = Ok(());
                egui::CollapsingHeader::new(title).default_open(open).show(ui, |ui| r = call_with_ui(vm, f, ui));
                r.map(|_| Value::None)
            }
            "columns" => {
                nargs(2, 2)?;
                let n = match args[0] {
                    Value::Int(n) if n > 0 && n < 64 => n as usize,
                    _ => return Err("ValueError: columns() needs a column count between 1 and 63".into()),
                };
                let f = args[1];
                let mut r = Ok(());
                ui.columns(n, |cols| r = call_with_uis(vm, f, cols));
                r.map(|_| Value::None)
            }
            "window" => {
                nargs(2, 2)?;
                let title = text_of(vm, args[0])?;
                let f = args[1];
                let mut w = egui::Window::new(title);
                if let (Some(a), Some(b)) = (opt_num(kw(vm, "width"), "width")?, opt_num(kw(vm, "height"), "height")?) {
                    w = w.default_size([a, b]);
                }
                if let (Some(x), Some(y)) = (opt_num(kw(vm, "x"), "x")?, opt_num(kw(vm, "y"), "y")?) {
                    w = w.default_pos([x, y]);
                }
                let ctx = ui.ctx().clone();
                let mut r = Ok(());
                w.show(&ctx, |ui| r = call_with_ui(vm, f, ui));
                r.map(|_| Value::None)
            }
            "canvas" => {
                nargs(0, 2)?;
                let avail = ui.available_size();
                let w = match args.first() {
                    Some(v) => num(*v, "width")?,
                    None => avail.x,
                };
                let h = match args.get(1) {
                    Some(v) => num(*v, "height")?,
                    None => avail.y,
                };
                let bg = match kw(vm, "background") {
                    None | Some(Value::None) => None,
                    Some(c) => Some(color(vm, c)?),
                };
                let (resp, painter) = ui.allocate_painter(Vec2::new(w.max(1.0), h.max(1.0)), Sense::click_and_drag());
                if let Some(bg) = bg {
                    painter.rect_filled(resp.rect, 0.0, bg);
                }
                let c = CanvasRef {
                    painter,
                    rect: resp.rect,
                    hover: resp.hover_pos(),
                    clicked: resp.clicked(),
                    down: resp.is_pointer_button_down_on(),
                    frame: STATE.with(|s| s.borrow().frame),
                };
                let hd = Handle { ty: &CANVAS_TYPE, data: Box::new(c) };
                Ok(Value::Obj(vm.heap.alloc(Obj::Handle(Box::new(hd)))))
            }
            "key_down" | "key_pressed" => {
                nargs(1, 1)?;
                let k = text_of(vm, args[0])?;
                Ok(Value::Bool(key_state(ui.ctx(), &k, name == "key_pressed")?))
            }
            "mouse" => {
                nargs(0, 0)?;
                match ui.ctx().input(|i| i.pointer.hover_pos()) {
                    Some(p) => Ok(vm.heap.tuple(vec![Value::Float(p.x as f64), Value::Float(p.y as f64)])),
                    None => Ok(Value::None),
                }
            }
            "mouse_down" => {
                nargs(0, 1)?;
                let b = match args.first() {
                    Some(v) => text_of(vm, *v)?,
                    None => "left".into(),
                };
                let button = match b.as_str() {
                    "left" => egui::PointerButton::Primary,
                    "right" => egui::PointerButton::Secondary,
                    "middle" => egui::PointerButton::Middle,
                    other => return Err(format!("ValueError: unknown mouse button '{}' (left, right, middle)", other)),
                };
                Ok(Value::Bool(ui.ctx().input(|i| i.pointer.button_down(button))))
            }
            "clicked" => {
                nargs(0, 0)?;
                Ok(Value::Bool(ui.ctx().input(|i| i.pointer.primary_clicked())))
            }
            "dt" => {
                nargs(0, 0)?;
                Ok(Value::Float(ui.ctx().input(|i| i.stable_dt) as f64))
            }
            "time" => {
                nargs(0, 0)?;
                Ok(Value::Float(ui.ctx().input(|i| i.time)))
            }
            "fps" => {
                nargs(0, 0)?;
                let dt = ui.ctx().input(|i| i.stable_dt).max(1e-6);
                Ok(Value::Int((1.0 / dt).round() as i64))
            }
            "width" | "height" => {
                nargs(0, 0)?;
                let s = ui.available_size();
                Ok(Value::Float(if name == "width" { s.x } else { s.y } as f64))
            }
            "close" => {
                nargs(0, 0)?;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                Ok(Value::None)
            }
            "repaint" => {
                nargs(0, 0)?;
                ui.ctx().request_repaint();
                Ok(Value::None)
            }
            "set_dark" => {
                nargs(1, 1)?;
                let d = vm.truthy(args[0])?;
                ui.ctx().set_visuals(if d { egui::Visuals::dark() } else { egui::Visuals::light() });
                Ok(Value::None)
            }
            _ => unreachable!("listed in UI_METHODS"),
        }
    })())
}

// ---------------------------------------------------------------- canvas

fn canvas_of<'a>(g: Gc) -> Result<&'a CanvasRef, String> {
    let c = match g.get() {
        Obj::Handle(h) => h.get::<CanvasRef>(),
        _ => None,
    }
    .ok_or("TypeError: not a canvas")?;
    if c.frame != STATE.with(|s| s.borrow().frame) {
        return Err("RuntimeError: a canvas is only usable during the frame it was made in".into());
    }
    Ok(c)
}

fn canvas_attr(vm: &mut Vm, g: Gc, name: &str) -> Option<R> {
    let c = match g.get() {
        Obj::Handle(h) => h.get::<CanvasRef>()?,
        _ => return None,
    };
    let _ = vm;
    Some(Ok(match name {
        "width" => Value::Float(c.rect.width() as f64),
        "height" => Value::Float(c.rect.height() as f64),
        _ => return None,
    }))
}

const CANVAS_METHODS: &[&str] = &["rect", "circle", "line", "polygon", "text", "image", "clear", "mouse", "clicked", "down"];

fn canvas_method(vm: &mut Vm, g: Gc, name: &str, args: &[Value]) -> Option<R> {
    if !CANVAS_METHODS.contains(&name) {
        return None;
    }
    Some((|| -> R {
        let c = canvas_of(g)?;
        let o = c.rect.min;
        let at = |x: f32, y: f32| Pos2::new(o.x + x, o.y + y);
        let n = |i: usize, what: &str| -> Result<f32, String> { num(arg(args, i, name)?, what) };
        let fill = |vm: &mut Vm| truthy_kw(vm, "fill", true);
        let p = &c.painter;
        match name {
            "rect" => {
                let r = Rect::from_min_size(at(n(0, "x")?, n(1, "y")?), Vec2::new(n(2, "w")?, n(3, "h")?));
                let col = match args.get(4) {
                    Some(v) => color(vm, *v)?,
                    None => color_kw(vm, "color", Color32::WHITE)?,
                };
                let radius = opt_num(kw(vm, "radius"), "radius")?.unwrap_or(0.0);
                let width = opt_num(kw(vm, "width"), "width")?.unwrap_or(1.0);
                if fill(vm)? {
                    p.rect_filled(r, radius, col);
                } else {
                    p.rect_stroke(r, radius, Stroke::new(width, col), egui::StrokeKind::Inside);
                }
            }
            "circle" => {
                let center = at(n(0, "x")?, n(1, "y")?);
                let radius = n(2, "radius")?;
                let col = match args.get(3) {
                    Some(v) => color(vm, *v)?,
                    None => color_kw(vm, "color", Color32::WHITE)?,
                };
                let width = opt_num(kw(vm, "width"), "width")?.unwrap_or(1.0);
                if fill(vm)? {
                    p.circle_filled(center, radius, col);
                } else {
                    p.circle_stroke(center, radius, Stroke::new(width, col));
                }
            }
            "line" => {
                let a = at(n(0, "x1")?, n(1, "y1")?);
                let b = at(n(2, "x2")?, n(3, "y2")?);
                let col = match args.get(4) {
                    Some(v) => color(vm, *v)?,
                    None => color_kw(vm, "color", Color32::WHITE)?,
                };
                let width = opt_num(kw(vm, "width"), "width")?.unwrap_or(2.0);
                p.line_segment([a, b], Stroke::new(width, col));
            }
            "polygon" => {
                let pts = vm.to_vec(arg(args, 0, name)?)?;
                let mut points = Vec::with_capacity(pts.len());
                for pt in pts {
                    let xy = vm.to_vec(pt)?;
                    if xy.len() != 2 {
                        return Err("TypeError: polygon() points must be (x, y) pairs".into());
                    }
                    points.push(at(num(xy[0], "x")?, num(xy[1], "y")?));
                }
                let col = match args.get(1) {
                    Some(v) => color(vm, *v)?,
                    None => color_kw(vm, "color", Color32::WHITE)?,
                };
                let width = opt_num(kw(vm, "width"), "width")?.unwrap_or(1.0);
                if fill(vm)? {
                    p.add(egui::Shape::convex_polygon(points, col, Stroke::NONE));
                } else {
                    p.add(egui::Shape::closed_line(points, Stroke::new(width, col)));
                }
            }
            "text" => {
                let pos = at(n(0, "x")?, n(1, "y")?);
                let t = text_of(vm, arg(args, 2, name)?)?;
                let col = match args.get(3) {
                    Some(v) => color(vm, *v)?,
                    None => color_kw(vm, "color", Color32::WHITE)?,
                };
                let size = opt_num(kw(vm, "size"), "size")?.unwrap_or(16.0);
                let align = match kw(vm, "align") {
                    Some(v) => text_of(vm, v)?,
                    None => "left".into(),
                };
                let anchor = match align.as_str() {
                    "center" => Align2::CENTER_TOP,
                    "right" => Align2::RIGHT_TOP,
                    _ => Align2::LEFT_TOP,
                };
                p.text(pos, anchor, t, FontId::proportional(size), col);
            }
            "image" => {
                let path = text_of(vm, arg(args, 0, name)?)?;
                let tex = texture(p.ctx(), &path)?;
                let [tw, th] = tex.size();
                let (x, y) = (n(1, "x")?, n(2, "y")?);
                let w = match args.get(3) {
                    Some(v) => num(*v, "w")?,
                    None => tw as f32,
                };
                let h = match args.get(4) {
                    Some(v) => num(*v, "h")?,
                    None => w * th as f32 / tw as f32,
                };
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                p.image(tex.id(), Rect::from_min_size(at(x, y), Vec2::new(w, h)), uv, Color32::WHITE);
            }
            "clear" => {
                let col = match args.first() {
                    Some(v) => color(vm, *v)?,
                    None => Color32::BLACK,
                };
                p.rect_filled(c.rect, 0.0, col);
            }
            "mouse" => {
                return Ok(match c.hover {
                    Some(h) => vm.heap.tuple(vec![Value::Float((h.x - o.x) as f64), Value::Float((h.y - o.y) as f64)]),
                    None => Value::None,
                })
            }
            "clicked" => return Ok(Value::Bool(c.clicked)),
            "down" => return Ok(Value::Bool(c.down)),
            _ => unreachable!("listed in CANVAS_METHODS"),
        }
        Ok(Value::None)
    })())
}
