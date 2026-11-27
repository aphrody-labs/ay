//! One tab: a Blitz document with Boa scripts, laid out, painted by vello_cpu and driven by input events.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{BaseDocument, Document, DocumentConfig, NodeId};
use blitz_html::{DocumentHtmlParser, HtmlProvider};
use blitz_traits::events::{
    BlitzInputEvent, BlitzKeyEvent, BlitzPointerEvent, DomEvent, DomEventData, BlitzPointerId, BlitzWheelDelta, BlitzWheelEvent, KeyState, MouseEventButton,
    MouseEventButtons, PointerCoords, PointerDetails, UiEvent,
};
use blitz_traits::shell::{ColorScheme, Viewport};
use blitz_vibey_script::ScriptDocument;
use keyboard_types::{Code, Key, Location, Modifiers, NamedKey};
use url::Url;

use crate::fonts;
use crate::net::{BlitzNet, Navigations, Net, ScriptNet};

const KS_JS: &str = include_str!("ks.js");
/// vello_cpu addresses pixels with u16.
const MAX_PIXELS: u32 = u16::MAX as u32;

pub struct Env {
    pub net: Arc<Net>,
    pub navigations: Arc<Mutex<Vec<(usize, Url)>>>,
}

pub struct Page {
    pub target_id: String,
    pub loader_id: String,
    pub url: String,
    pub context_id: u64,
    pub doc: ScriptDocument,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
    pub init_scripts: Vec<(String, String)>,
    pub history: Vec<String>,
    pub history_index: usize,
}

pub enum Format {
    Png,
    Jpeg(u8),
}

pub struct Clip {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

fn js_string(s: &str) -> String {
    serde_json::Value::from(s).to_string()
}

impl Page {
    pub fn new(env: &Env, target_id: String, context_id: u64, width: u32, height: u32) -> Self {
        let url = Url::parse("about:blank").unwrap();
        let doc = build(env, &url, "", width, height, 1.0, &[]);
        Self {
            loader_id: target_id.clone(),
            target_id,
            url: url.to_string(),
            context_id,
            doc,
            width,
            height,
            scale: 1.0,
            init_scripts: Vec::new(),
            history: vec![url.to_string()],
            history_index: 0,
        }
    }

    pub fn doc_id(&self) -> usize {
        self.doc.inner().id()
    }

    /// Fetch `url` and replace the document (scripts not yet run: see [`Page::load`]).
    /// Returns an error text for a failed fetch, as `Page.navigate`'s `errorText`.
    pub fn open(&mut self, env: &Env, url: &str, context_id: u64) -> Result<(), String> {
        let url = Url::parse(url).map_err(|e| format!("net::ERR_INVALID_URL ({e})"))?;
        let response = env.net.get(&url).map_err(|e| format!("net::ERR_FAILED ({e})"))?;
        let final_url = Url::parse(&response.url).unwrap_or(url);
        let html = String::from_utf8_lossy(&response.body);
        self.set_content(env, &final_url, &html, context_id);
        Ok(())
    }

    pub fn set_content(&mut self, env: &Env, url: &Url, html: &str, context_id: u64) {
        self.doc = build(env, url, html, self.width, self.height, self.scale, &self.init_scripts);
        self.url = url.to_string();
        self.context_id = context_id;
        self.loader_id = format!("{:016X}", context_id);
    }

    pub fn push_history(&mut self) {
        self.history.truncate(self.history_index + 1);
        if self.history.last() != Some(&self.url) {
            self.history.push(self.url.clone());
        }
        self.history_index = self.history.len() - 1;
    }

    /// Run the document's scripts (DOMContentLoaded and load fire in there), then wait up to `budget`
    /// for its render-blocking resources.
    pub fn load(&mut self, budget: Duration) {
        self.doc.execute_scripts();
        let deadline = Instant::now() + budget;
        loop {
            self.pump();
            if !self.doc.inner().has_pending_critical_resources() || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        self.resolve();
    }

    /// Deliver loaded resources and run due timers.
    pub fn pump(&mut self) -> bool {
        self.doc.inner_mut().handle_messages();
        self.doc.run_due_timers()
    }

    pub fn resolve(&mut self) {
        self.doc.inner_mut().resolve(0.0);
    }

    /// Run `__ks.<call>` and parse its JSON answer (`""` means no answer).
    pub fn ks(&mut self, call: &str) -> Result<Option<serde_json::Value>, String> {
        let out = self.doc.evaluate_to_string(&format!("__ks.{call}"))?;
        if out.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&out).map(Some).map_err(|e| format!("bad page answer: {e}"))
    }

    /// `__ks.<call>` where the call answers with a plain string.
    pub fn ks_text(&mut self, call: &str) -> Result<String, String> {
        self.doc.evaluate_to_string(&format!("__ks.{call}"))
    }

    /// Evaluate through `__ks.evaluate`/`__ks.call` and wait (pumping timers and resources) for a
    /// pending promise when `awaitPromise` was asked.
    pub fn run(&mut self, call: &str, timeout: Duration) -> Result<serde_json::Value, String> {
        let answer = self.ks(call)?.unwrap_or(serde_json::Value::Null);
        let Some(pending) = answer.get("pending").and_then(|p| p.as_u64()) else {
            return Ok(answer);
        };
        let deadline = Instant::now() + timeout;
        loop {
            self.doc.run_jobs();
            if let Some(result) = self.ks(&format!("take({pending})"))? {
                return Ok(result);
            }
            if Instant::now() >= deadline {
                return Err("Promise was not settled in time".into());
            }
            if !self.pump() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    pub fn evaluate(&mut self, expression: &str, options: &serde_json::Value, timeout: Duration) -> Result<serde_json::Value, String> {
        self.run(&format!("evaluate({}, {options})", js_string(expression)), timeout)
    }

    pub fn call_function(
        &mut self,
        object_id: Option<&str>,
        declaration: &str,
        args: &serde_json::Value,
        options: &serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        let object = object_id.map(js_string).unwrap_or_else(|| "null".into());
        self.run(&format!("call({object}, {}, {args}, {options})", js_string(declaration)), timeout)
    }

    pub fn title(&mut self) -> String {
        self.ks_text("title()").unwrap_or_default()
    }

    pub fn set_viewport(&mut self, width: u32, height: u32, scale: f32) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.scale = if scale > 0.0 { scale } else { 1.0 };
        let viewport = viewport(self.width, self.height, self.scale);
        self.doc.inner_mut().set_viewport(viewport);
    }

    /// The laid-out size of the whole document in CSS pixels.
    pub fn content_size(&mut self) -> (f64, f64) {
        self.resolve();
        let doc = self.doc.inner();
        let size = doc.root_element().final_layout().size;
        ((size.width as f64).max(self.width as f64), (size.height as f64).max(self.height as f64))
    }

    pub fn scroll(&mut self) -> (f64, f64) {
        let scroll = self.doc.inner().viewport_scroll();
        (scroll.x, scroll.y)
    }

    pub fn screenshot(&mut self, format: Format, clip: Option<Clip>, full_page: bool) -> Result<Vec<u8>, String> {
        let (content_width, content_height) = self.content_size();
        let clip = clip.unwrap_or_else(|| {
            let (x, y) = if full_page { (0.0, 0.0) } else { self.scroll() };
            let (width, height) = if full_page { (content_width, content_height) } else { (self.width as f64, self.height as f64) };
            Clip { x, y, width, height, scale: 1.0 }
        });
        let scale = self.scale as f64 * if clip.scale > 0.0 { clip.scale } else { 1.0 };
        let width = ((clip.width * scale).ceil() as u32).clamp(1, MAX_PIXELS);
        let height = ((clip.height * scale).ceil() as u32).clamp(1, MAX_PIXELS);
        let (x, y) = ((clip.x * scale).max(0.0) as u32, (clip.y * scale).max(0.0) as u32);
        // The painter adds the viewport scroll itself; the clip is in page coordinates.
        let (scroll_x, scroll_y) = self.scroll();
        let (x, y) = (x.saturating_sub((scroll_x * scale) as u32), y.saturating_sub((scroll_y * scale) as u32));
        let mut doc = self.doc.inner_mut();
        let mut pixels = anyrender::render_to_buffer::<VelloCpuImageRenderer, _>(
            |scene| blitz_paint::paint_scene(scene, &mut doc, scale, width, height, x, y),
            width,
            height,
        );
        drop(doc);
        // Premultiplied RGBA over an opaque white canvas.
        for px in pixels.chunks_exact_mut(4) {
            let transparent = 255 - px[3];
            px[0] = px[0].saturating_add(transparent);
            px[1] = px[1].saturating_add(transparent);
            px[2] = px[2].saturating_add(transparent);
            px[3] = 255;
        }
        encode(format, &pixels, width, height)
    }

    pub fn mouse(&mut self, kind: &str, x: f32, y: f32, button: &str, buttons: u8, delta: (f64, f64), modifiers: u64) {
        self.resolve();
        let coords = PointerCoords { page_x: x, page_y: y, screen_x: x, screen_y: y, client_x: x, client_y: y };
        let mods = modifiers_of(modifiers);
        let buttons = MouseEventButtons::from_bits_truncate(buttons);
        let button = match button {
            "right" => MouseEventButton::Secondary,
            "middle" => MouseEventButton::Auxiliary,
            _ => MouseEventButton::Main,
        };
        let pointer = |buttons: MouseEventButtons| BlitzPointerEvent {
            id: BlitzPointerId::Mouse,
            is_primary: true,
            coords,
            button,
            buttons,
            mods,
            details: PointerDetails::default(),
            element: Default::default(),
            active_pointers: Default::default(),
        };
        let event = match kind {
            "mousePressed" => {
                let pressed = if buttons.is_empty() { button_bits(button) } else { buttons };
                UiEvent::PointerDown(pointer(pressed))
            }
            "mouseReleased" => UiEvent::PointerUp(pointer(buttons)),
            "mouseWheel" => UiEvent::Wheel(BlitzWheelEvent {
                delta: BlitzWheelDelta::Pixels(-delta.0, -delta.1),
                coords,
                buttons,
                mods,
                element: Default::default(),
            }),
            _ => UiEvent::PointerMove(pointer(buttons)),
        };
        self.doc.handle_ui_event(event);
        self.resolve();
    }

    /// Typed text is appended to the focused text control's live value (kitesurf's blitz-dom lets the
    /// embedder own it), then an `input` event goes through the event driver. Returns false when no
    /// text control has focus.
    pub fn insert_text(&mut self, text: &str) -> Result<bool, String> {
        self.edit_text(text, 0)
    }

    fn edit_text(&mut self, text: &str, back: usize) -> Result<bool, String> {
        let Some(answer) = self.ks(&format!("editText({}, {back})", js_string(text)))? else {
            return Ok(false);
        };
        let (Some(id), Some(value)) = (answer["node"].as_u64(), answer["value"].as_str()) else {
            return Ok(false);
        };
        let event = DomEvent::new(NodeId::from_u64(id - 1), DomEventData::Input(BlitzInputEvent { value: value.to_string() }));
        self.doc.dispatch_dom_event(event);
        self.resolve();
        Ok(true)
    }

    pub fn key(&mut self, kind: &str, key: &str, code: &str, text: Option<&str>, modifiers: u64, auto_repeat: bool) {
        let state = if kind == "keyUp" { KeyState::Released } else { KeyState::Pressed };
        // Blitz's editor would type into its display buffer, which kitesurf's blitz-dom re-derives from
        // the embedder-owned value: printable text and Backspace in a text control are edits of that value.
        if state == KeyState::Pressed {
            // `keyDown` is `rawKeyDown` plus `char`: text arrives with one or the other.
            let printable = text.filter(|t| kind != "rawKeyDown" && !t.is_empty() && !t.chars().any(char::is_control));
            let edited = match (printable, key) {
                (Some(t), _) => self.edit_text(t, 0),
                (None, "Backspace") => self.edit_text("", 1),
                _ => Ok(false),
            };
            if edited.unwrap_or(false) {
                return;
            }
        }
        if kind == "char" {
            return;
        }
        let key: Key = key.parse().unwrap_or_else(|_| match text {
            Some(t) if !t.is_empty() => Key::Character(t.to_string()),
            _ => Key::Named(NamedKey::Unidentified),
        });
        let event = BlitzKeyEvent {
            key,
            code: code.parse().unwrap_or(Code::Unidentified),
            modifiers: modifiers_of(modifiers),
            location: Location::Standard,
            is_auto_repeating: auto_repeat,
            is_composing: false,
            state,
            text: text.filter(|t| !t.is_empty() && state == KeyState::Pressed).map(Into::into),
        };
        self.doc.handle_ui_event(if state == KeyState::Pressed { UiEvent::KeyDown(event) } else { UiEvent::KeyUp(event) });
        self.resolve();
    }
}

fn button_bits(button: MouseEventButton) -> MouseEventButtons {
    match button {
        MouseEventButton::Secondary => MouseEventButtons::Secondary,
        MouseEventButton::Auxiliary => MouseEventButtons::Auxiliary,
        _ => MouseEventButtons::Primary,
    }
}

/// CDP modifier bits: Alt=1, Ctrl=2, Meta=4, Shift=8.
fn modifiers_of(bits: u64) -> Modifiers {
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::ALT, bits & 1 != 0);
    mods.set(Modifiers::CONTROL, bits & 2 != 0);
    mods.set(Modifiers::META, bits & 4 != 0);
    mods.set(Modifiers::SHIFT, bits & 8 != 0);
    mods
}

fn viewport(width: u32, height: u32, scale: f32) -> Viewport {
    Viewport::new((width as f32 * scale) as u32, (height as f32 * scale) as u32, scale, ColorScheme::Light)
}

fn build(env: &Env, url: &Url, html: &str, width: u32, height: u32, scale: f32, init_scripts: &[(String, String)]) -> ScriptDocument {
    let config = DocumentConfig {
        viewport: Some(viewport(width, height, scale)),
        base_url: Some(url.to_string()),
        net_provider: Some(Arc::new(BlitzNet(Arc::clone(&env.net)))),
        navigation_provider: Some(Arc::new(Navigations(Arc::clone(&env.navigations)))),
        html_parser_provider: Some(Arc::new(HtmlProvider)),
        font_ctx: Some(fonts::context()),
        ..Default::default()
    };
    let mut doc = BaseDocument::new(config);
    {
        let mut mutr = doc.mutate();
        DocumentHtmlParser::parse_into_mutator_with_scripting(&mut mutr, html, true);
    }
    let mut doc = ScriptDocument::from_base_document(doc).without_timer_thread().with_fetcher(ScriptNet(Arc::clone(&env.net)));
    if let Err(error) = doc.evaluate_to_string(KS_JS) {
        log::error!("installing the DevTools page helper failed: {error}");
    }
    for (_, source) in init_scripts {
        let _ = doc.evaluate_to_string(source);
    }
    doc
}

fn encode(format: Format, rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match format {
        Format::Png => {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
            writer.write_image_data(rgba).map_err(|e| e.to_string())?;
            writer.finish().map_err(|e| e.to_string())?;
        }
        Format::Jpeg(quality) => {
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            let encoder = jpeg_encoder::Encoder::new(&mut out, quality.clamp(1, 100));
            encoder
                .encode(&rgb, width as u16, height as u16, jpeg_encoder::ColorType::Rgb)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(out)
}
