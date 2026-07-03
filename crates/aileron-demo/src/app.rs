/// aileron-demo — sandboxed GTK4 article summarizer.
mod frontends;
pub(crate) mod tool_demo;

use gtk4::prelude::*;
use gtk4::{Button, Label};
use libadwaita::prelude::*;
use libadwaita::{
    ApplicationWindow, HeaderBar, OverlaySplitView, ToolbarView, ViewStack, ViewSwitcherSidebar,
    WindowTitle,
};
use relm4::{ComponentParts, ComponentSender, RelmApp, SimpleComponent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zbus::zvariant::{Fd, OwnedFd, OwnedObjectPath, OwnedValue, Type, Value};

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const LANGUAGE_IFACE: &str = "org.freedesktop.portal.Language";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";
const SESSION_IFACE: &str = "org.freedesktop.portal.Session";
const SPEECH_IFACE: &str = "org.freedesktop.portal.Speech";
const VISION_IFACE: &str = "org.freedesktop.portal.Vision";

static PORTAL_CONNECTION: OnceLock<zbus::blocking::Connection> = OnceLock::new();

type PortalOptions = HashMap<String, OwnedValue>;

fn empty_options() -> PortalOptions {
    HashMap::new()
}

fn text_shorthand_json(text: &str) -> String {
    serde_json::json!([{ "type": "input_text", "text": text }]).to_string()
}

fn portal_connection() -> zbus::Result<zbus::blocking::Connection> {
    if let Some(conn) = PORTAL_CONNECTION.get() {
        return Ok(conn.clone());
    }

    let conn = zbus::blocking::Connection::session()?;
    if PORTAL_CONNECTION.set(conn.clone()).is_ok() {
        Ok(conn)
    } else {
        Ok(PORTAL_CONNECTION
            .get()
            .expect("portal connection was set")
            .clone())
    }
}

fn string_option_value(value: &str) -> OwnedValue {
    OwnedValue::try_from(Value::from(value.to_string())).expect("string options are valid values")
}

fn generation_options(
    maximum_response_tokens: i64,
    source_language_hint: &str,
    target_language_hint: &str,
) -> PortalOptions {
    let mut options = HashMap::new();
    options.insert(
        "maximum_response_tokens".to_string(),
        OwnedValue::from(maximum_response_tokens),
    );
    options.insert(
        "source_language_hint".to_string(),
        string_option_value(source_language_hint),
    );
    options.insert(
        "target_language_hint".to_string(),
        string_option_value(target_language_hint),
    );
    options
}

fn create_public_session(
    proxy: &zbus::blocking::Proxy<'_>,
    use_case: &str,
    instructions: &str,
) -> anyhow::Result<OwnedObjectPath> {
    let request_handle: OwnedObjectPath = proxy.call(
        "CreateSession",
        &("", use_case, instructions, empty_options()),
    )?;
    let mut results = wait_request_response(&request_handle)?;
    let session_handle = results
        .remove("session_handle")
        .ok_or_else(|| anyhow::anyhow!("CreateSession response omitted session_handle"))?;
    OwnedObjectPath::try_from(session_handle)
        .map_err(|e| anyhow::anyhow!("CreateSession returned invalid session handle: {e}"))
}

fn wait_request_response(
    request_handle: &OwnedObjectPath,
) -> anyhow::Result<HashMap<String, OwnedValue>> {
    let conn = portal_connection()?;
    let proxy =
        zbus::blocking::Proxy::new(&conn, PORTAL_BUS, request_handle.as_str(), REQUEST_IFACE)?;
    let mut response_iter = proxy.receive_signal("Response")?;
    let msg = response_iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("request closed without a Response signal"))?;
    let (response, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;

    if response != 0 {
        let message = results
            .get("error")
            .and_then(|value| String::try_from(value.clone()).ok())
            .unwrap_or_else(|| format!("portal request failed with response {response}"));
        anyhow::bail!(message);
    }

    Ok(results)
}

fn wait_request_success(request_handle: &OwnedObjectPath) -> anyhow::Result<()> {
    wait_request_response(request_handle).map(|_| ())
}

fn close_public_session(session_handle: &OwnedObjectPath) -> zbus::Result<()> {
    let conn = portal_connection()?;
    let proxy =
        zbus::blocking::Proxy::new(&conn, PORTAL_BUS, session_handle.as_str(), SESSION_IFACE)?;
    proxy.call("Close", &())
}

fn cached_session_handle(session_handle: String) -> anyhow::Result<OwnedObjectPath> {
    OwnedObjectPath::try_from(session_handle)
        .map_err(|e| anyhow::anyhow!("cached portal session handle is invalid: {e}"))
}

#[derive(Debug)]
pub enum AppMsg {}

pub struct AppModel;

pub struct AppWidgets;

pub fn run() {
    libadwaita::init().expect("failed to initialise libadwaita");
    let app = RelmApp::new("org.aileron.Demo");
    app.run::<AppModel>(());
}

impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();
    type Widgets = AppWidgets;
    type Root = ApplicationWindow;

    fn init_root() -> Self::Root {
        ApplicationWindow::builder()
            .title("Aileron Demo")
            .default_width(860)
            .default_height(560)
            .build()
    }

    fn init(
        (): Self::Init,
        window: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        build_window(&window);
        ComponentParts {
            model: AppModel,
            widgets: AppWidgets,
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {}
    }
}

fn build_window(window: &ApplicationWindow) {
    let stack = ViewStack::new();

    let overview_page = stack.add_titled(
        &frontends::overview::build_page(&stack),
        Some("overview"),
        "Lab overview",
    );
    overview_page.set_icon_name(Some("view-dashboard-symbolic"));
    let text_page = stack.add_titled(&frontends::text::build_page(), Some("text"), "Text lab");
    text_page.set_icon_name(Some("text-x-generic-symbolic"));
    let prediction_page = stack.add_titled(
        &frontends::prediction::build_page(),
        Some("predict"),
        "Prediction lab",
    );
    prediction_page.set_icon_name(Some("insert-text-symbolic"));
    let chat_page = stack.add_titled(&frontends::chat::build_page(), Some("chat"), "Chat lab");
    chat_page.set_icon_name(Some("user-available-symbolic"));
    let tool_page = stack.add_titled(&frontends::tool::build_page(), Some("tools"), "Tool lab");
    tool_page.set_icon_name(Some("applications-system-symbolic"));
    let speech_page = stack.add_titled(
        &frontends::speech::build_page(),
        Some("speech"),
        "Speech lab",
    );
    speech_page.set_icon_name(Some("audio-input-microphone-symbolic"));
    let vision_page = stack.add_titled(
        &frontends::vision::build_page(),
        Some("vision"),
        "Vision lab",
    );
    vision_page.set_icon_name(Some("image-x-generic-symbolic"));
    let embed_page = stack.add_titled(
        &frontends::embedding::build_page(),
        Some("embed"),
        "Embeddings",
    );
    embed_page.set_icon_name(Some("emblem-documents-symbolic"));
    stack.set_visible_child_name("overview");

    let sidebar = ViewSwitcherSidebar::builder().stack(&stack).build();

    let split_view = OverlaySplitView::new();
    split_view.set_min_sidebar_width(190.0);
    split_view.set_max_sidebar_width(240.0);
    split_view.set_show_sidebar(true);

    let sidebar_header = HeaderBar::new();
    let hide_sidebar_button = Button::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle sidebar")
        .build();
    {
        let split_view = split_view.clone();
        hide_sidebar_button.connect_clicked(move |_| {
            split_view.set_show_sidebar(false);
        });
    }
    sidebar_header.pack_start(&hide_sidebar_button);
    sidebar_header.set_title_widget(Some(&Label::new(None)));
    let sidebar_view = ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.set_content(Some(&sidebar));

    let content_header = HeaderBar::new();
    let show_sidebar_button = Button::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle sidebar")
        .build();
    {
        let split_view = split_view.clone();
        show_sidebar_button.connect_clicked(move |_| {
            split_view.set_show_sidebar(true);
        });
    }
    content_header.pack_start(&show_sidebar_button);
    let title = WindowTitle::builder()
        .title("Lab overview")
        .subtitle("Aileron demo")
        .build();
    let title_for_stack = title.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        title_for_stack.set_title(match stack.visible_child_name().as_deref() {
            Some("overview") => "Lab overview",
            Some("text") => "Text lab",
            Some("predict") => "Prediction lab",
            Some("chat") => "Chat lab",
            Some("tools") => "Tool lab",
            Some("speech") => "Speech lab",
            Some("vision") => "Vision lab",
            Some("embed") => "Embeddings",
            _ => "Aileron demo",
        });
    });
    content_header.set_title_widget(Some(&title));
    let content_view = ToolbarView::new();
    content_view.add_top_bar(&content_header);
    content_view.set_content(Some(&stack));

    split_view.set_sidebar(Some(&sidebar_view));
    split_view.set_content(Some(&content_view));
    {
        let show_sidebar_button = show_sidebar_button.clone();
        split_view.connect_show_sidebar_notify(move |split_view| {
            show_sidebar_button.set_visible(!split_view.shows_sidebar());
        });
    }
    show_sidebar_button.set_visible(false);

    window.set_content(Some(&split_view));
}

fn fetch_article_text(url: &str) -> anyhow::Result<String> {
    let response = reqwest::blocking::get(url)?;
    let html = response.text()?;
    Ok(strip_html(&html))
}

fn strip_html(html: &str) -> String {
    // Drop <script>…</script> and <style>…</style> blocks (case-insensitive,
    // including tags with attributes like <style media="screen">).
    let mut s = html.to_string();
    for tag in &["script", "style"] {
        let open = format!("<{}", tag);
        let close = format!("</{}>", tag);
        let s_lower = s.to_lowercase();
        let mut out = String::with_capacity(s.len());
        let mut pos = 0;
        let bytes = s_lower.as_bytes();
        let ob = open.as_bytes();
        let cb = close.as_bytes();
        while pos < bytes.len() {
            if let Some(rel) = s_lower[pos..].find(open.as_str()) {
                out.push_str(&s[pos..pos + rel]);
                let after_open = pos + rel;
                if let Some(rel2) = s_lower[after_open..].find(close.as_str()) {
                    pos = after_open + rel2 + cb.len();
                } else {
                    pos = bytes.len();
                }
                let _ = ob;
            } else {
                out.push_str(&s[pos..]);
                break;
            }
        }
        s = out;
    }

    // Strip remaining tags; emit newline for block-level close tags.
    let block_close = [
        "</p>",
        "</div>",
        "</li>",
        "</h1>",
        "</h2>",
        "</h3>",
        "</h4>",
        "</article>",
        "</section>",
        "</header>",
        "</nav>",
    ];
    let s_lower = s.to_lowercase();
    let mut output = String::with_capacity(s.len());
    let mut inside_tag = false;
    let mut tag_buf = String::new();
    for ch in s.chars() {
        match ch {
            '<' => {
                inside_tag = true;
                tag_buf.clear();
                tag_buf.push('<');
            }
            '>' => {
                inside_tag = false;
                tag_buf.push('>');
                let tb = tag_buf.to_lowercase();
                if block_close.iter().any(|t| tb.starts_with(t)) {
                    output.push('\n');
                } else {
                    output.push(' ');
                }
                tag_buf.clear();
            }
            _ if inside_tag => tag_buf.push(ch),
            _ => output.push(ch),
        }
    }
    let _ = s_lower;

    // Return from the first substantial paragraph (>200 chars) that doesn't
    // look like boilerplate (cookie notices, nav menus, etc.).
    let boilerplate_hints = ["cookie", "privacy", "login", "register", "newsletter"];
    let paragraphs: Vec<&str> = output
        .split('\n')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    let article_start = paragraphs
        .iter()
        .position(|p| {
            let lower = p.to_lowercase();
            p.len() > 200 && !boilerplate_hints.iter().any(|h| lower.contains(h))
        })
        .unwrap_or(0);

    paragraphs[article_start..]
        .iter()
        .flat_map(|p| [*p, "\n\n"])
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .pipe(decode_html_entities)
}

/// Decode common HTML entities. stdlib-only, no external crate.
fn decode_html_entities(s: String) -> String {
    // Named entities ordered longest-first within each group to avoid
    // partial matches (e.g. &amp; before &a).
    const NAMED: &[(&str, &str)] = &[
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&apos;", "'"),
        ("&nbsp;", " "),
        ("&mdash;", "—"),
        ("&ndash;", "–"),
        ("&laquo;", "«"),
        ("&raquo;", "»"),
        ("&hellip;", "…"),
        ("&copy;", "©"),
        ("&reg;", "®"),
        ("&trade;", "™"),
    ];

    let mut result = s;
    // Named entities.
    for (entity, replacement) in NAMED {
        result = result.replace(entity, replacement);
    }
    // Decimal numeric entities: &#NNN;
    let mut out = String::with_capacity(result.len());
    let mut chars = result.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&' && chars.peek() == Some(&'#') {
            chars.next(); // consume '#'
            let mut digits = String::new();
            let hex = chars.peek() == Some(&'x') || chars.peek() == Some(&'X');
            if hex {
                chars.next();
            }
            while let Some(&d) = chars.peek() {
                if d == ';' {
                    chars.next();
                    break;
                }
                if d.is_ascii_alphanumeric() {
                    digits.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let codepoint = if hex {
                u32::from_str_radix(&digits, 16).ok()
            } else {
                digits.parse::<u32>().ok()
            };
            if let Some(c) = codepoint.and_then(char::from_u32) {
                out.push(c);
            } else {
                out.push_str(if hex { "&#x" } else { "&#" });
                out.push_str(&digits);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl Pipe for String {}

enum DemoEvent {
    Phase(DemoPhase),
    Status(String),
    Token(String),
    Json(String),
    Text(String),
    Error(String),
    Done,
}

enum ChatEvent {
    SessionReady(String),
    Draft(String),
    Response(GuidedChatResponse),
    Error(String),
    Done,
}

enum PredictionEvent {
    SessionReady {
        seq: u64,
        id: String,
    },
    Busy(u64),
    Suggestion {
        seq: u64,
        input_text: String,
        suggestions: Vec<String>,
    },
    Error {
        seq: u64,
        message: String,
        attempted_session: Option<String>,
    },
}

#[derive(Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct GuidedChatResponse {
    answer: String,
    memory: String,
    #[allow(dead_code)]
    confidence: i64,
}

#[derive(Clone, Copy)]
enum DemoMode {
    Summarize,
    Translate,
    Rephrase,
    Classify,
    Extract,
    Analyze,
}

impl DemoMode {
    fn labels() -> [&'static str; 6] {
        [
            DemoMode::Summarize.ready_label(),
            DemoMode::Translate.ready_label(),
            DemoMode::Rephrase.ready_label(),
            DemoMode::Classify.ready_label(),
            DemoMode::Extract.ready_label(),
            DemoMode::Analyze.ready_label(),
        ]
    }

    fn index(&self) -> u32 {
        match self {
            DemoMode::Summarize => 0,
            DemoMode::Translate => 1,
            DemoMode::Rephrase => 2,
            DemoMode::Classify => 3,
            DemoMode::Extract => 4,
            DemoMode::Analyze => 5,
        }
    }

    fn from_index(index: u32) -> Option<Self> {
        match index {
            0 => Some(DemoMode::Summarize),
            1 => Some(DemoMode::Translate),
            2 => Some(DemoMode::Rephrase),
            3 => Some(DemoMode::Classify),
            4 => Some(DemoMode::Extract),
            5 => Some(DemoMode::Analyze),
            _ => None,
        }
    }

    fn ready_label(&self) -> &'static str {
        match self {
            DemoMode::Summarize => "Summarize",
            DemoMode::Translate => "Translate",
            DemoMode::Rephrase => "Rephrase",
            DemoMode::Classify => "Classify",
            DemoMode::Extract => "Extract JSON",
            DemoMode::Analyze => "Analyze",
        }
    }

    fn busy_label(&self) -> &'static str {
        match self {
            DemoMode::Summarize => "Summarizing...",
            DemoMode::Translate => "Translating...",
            DemoMode::Rephrase => "Rephrasing...",
            DemoMode::Classify => "Classifying...",
            DemoMode::Extract => "Extracting JSON...",
            DemoMode::Analyze => "Analyzing...",
        }
    }

    fn initial_title(&self) -> &'static str {
        match self {
            DemoMode::Summarize => "Creating summary session",
            DemoMode::Translate => "Creating translation session",
            DemoMode::Rephrase => "Creating rephrase session",
            DemoMode::Classify => "Creating classification session",
            DemoMode::Extract => "Creating extraction session",
            DemoMode::Analyze => "Creating analysis session",
        }
    }

    fn initial_detail(&self) -> &'static str {
        match self {
            DemoMode::Summarize => "Opening a language.summarize session through the portal...",
            DemoMode::Translate => "Opening a language.translate session through the portal...",
            DemoMode::Rephrase => "Opening a language.rephrase session through the portal...",
            DemoMode::Classify => "Opening a language.classify session through the portal...",
            DemoMode::Extract => "Opening a language.extract session through the portal...",
            DemoMode::Analyze => "Opening a language.analyze session through the portal...",
        }
    }

    fn complete_title(&self) -> &'static str {
        match self {
            DemoMode::Summarize => "Summary complete",
            DemoMode::Translate => "Translation complete",
            DemoMode::Rephrase => "Rephrase complete",
            DemoMode::Classify => "Classification complete",
            DemoMode::Extract => "Extract JSON complete",
            DemoMode::Analyze => "Analysis complete",
        }
    }

    fn complete_detail(&self) -> &'static str {
        match self {
            DemoMode::Summarize => "The local model finished streaming its response.",
            DemoMode::Translate | DemoMode::Rephrase | DemoMode::Analyze => {
                "The local model returned the task result through StreamResponse."
            }
            DemoMode::Classify | DemoMode::Extract => {
                "The daemon validated the model output against the generated schema."
            }
        }
    }

    fn use_case(&self) -> &'static str {
        match self {
            DemoMode::Summarize => "language.summarize",
            DemoMode::Translate => "language.translate",
            DemoMode::Rephrase => "language.rephrase",
            DemoMode::Classify => "language.classify",
            DemoMode::Extract => "language.extract",
            DemoMode::Analyze => "language.analyze",
        }
    }

    fn instructions(&self) -> &'static str {
        match self {
            DemoMode::Summarize => "You summarize user-provided text clearly and concisely.",
            DemoMode::Translate => "You translate text accurately while preserving meaning.",
            DemoMode::Rephrase => "You rewrite text clearly while preserving meaning.",
            DemoMode::Classify => "You classify text into concise, useful categories.",
            DemoMode::Extract => "You extract concise, factual summary data as valid JSON.",
            DemoMode::Analyze => "You analyze text carefully and explain the important findings.",
        }
    }

    fn prompt(&self, text: &str) -> String {
        let trimmed = &text[..text.len().min(8192)];
        match self {
            DemoMode::Summarize => format!(
                "Summarize the following article in 3-5 sentences. Return only the summary. Do not repeat or answer the instruction/question:\n\n{trimmed}"
            ),
            DemoMode::Translate => format!(
                "Translate the following text into Spanish. Preserve names, numbers, formatting intent, and tone. Return only the translation:\n\n{trimmed}"
            ),
            DemoMode::Rephrase => format!(
                "Rephrase the following text to be clearer and more direct. Preserve the original meaning and important details. Return only the rewritten text:\n\n{trimmed}"
            ),
            DemoMode::Classify => format!(
                "Classify the following text by topic and intent. Choose concise labels and include a short rationale:\n\n{trimmed}"
            ),
            DemoMode::Extract => format!(
                "Summarize this article as structured data. Keep the summary short, include 3-5 key points, and set confidence from 0 to 100:\n\n{trimmed}"
            ),
            DemoMode::Analyze => format!(
                "Analyze the following text. Identify the main claim, supporting evidence, assumptions, and any risks or open questions. Keep the answer concise:\n\n{trimmed}"
            ),
        }
    }
}

enum DemoPhase {
    CreatingSession,
    WaitingForModel,
    RequestingStream,
    RequestingGuided,
    RequestingResponse,
}

impl DemoPhase {
    fn title(&self) -> &'static str {
        match self {
            DemoPhase::CreatingSession => "Creating session",
            DemoPhase::WaitingForModel => "Loading model",
            DemoPhase::RequestingStream => "Starting response",
            DemoPhase::RequestingGuided => "Requesting guided JSON",
            DemoPhase::RequestingResponse => "Requesting response",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            DemoPhase::CreatingSession => "Asking the portal to open a language model session...",
            DemoPhase::WaitingForModel => "Starting the local container if the model is cold...",
            DemoPhase::RequestingStream => "Sending the prompt and waiting for the first token...",
            DemoPhase::RequestingGuided => "Sending field guides and waiting for validated JSON...",
            DemoPhase::RequestingResponse => "Sending the prompt and waiting for the result...",
        }
    }

    fn is_active(&self) -> bool {
        true
    }
}

enum SpeechEvent {
    Phase(SpeechPhase),
    Transcript(String),
    AppendTranscript(String),
    Error(String),
    Done,
}

const LIVE_SPEECH_MIN_CHUNK_BYTES: u64 = 4 * 16_000 * 4;
const LIVE_SPEECH_POLL_MS: u64 = 500;

#[derive(Debug, Clone, Serialize, Type)]
struct ToolDefinitionDbus {
    name: String,
    description: String,
    schema_json: String,
}

#[derive(Debug, Clone, Deserialize, Type)]
struct ToolCallDbus {
    id: String,
    name: String,
    arguments_json: String,
}

#[derive(Debug, Clone, Serialize, Type)]
struct ToolResultDbus {
    id: String,
    content: String,
    content_json: String,
}

enum SpeechPhase {
    CreatingSession,
    LoadingModel,
    Transcribing,
    LiveRecording,
    LiveChunk,
    Finalizing,
}

impl SpeechPhase {
    fn title(&self) -> &'static str {
        match self {
            SpeechPhase::CreatingSession => "Creating Speech session",
            SpeechPhase::LoadingModel => "Loading Speech model",
            SpeechPhase::Transcribing => "Processing audio",
            SpeechPhase::LiveRecording => "Live transcription running",
            SpeechPhase::LiveChunk => "Processing live audio",
            SpeechPhase::Finalizing => "Finalizing transcript",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            SpeechPhase::CreatingSession => "Opening a Speech session through the portal...",
            SpeechPhase::LoadingModel => "Starting the local Speech container if it is cold...",
            SpeechPhase::Transcribing => "Sending recorded microphone audio to the Speech model...",
            SpeechPhase::LiveRecording => {
                "Recording microphone audio. Interim text may change after the final pass."
            }
            SpeechPhase::LiveChunk => "Sending the newest audio chunk for provisional text...",
            SpeechPhase::Finalizing => {
                "Replacing provisional chunks with one full-recording pass..."
            }
        }
    }
}

enum VisionEvent {
    Phase(VisionPhase),
    Description(String),
    Ocr(String),
    Segments(Vec<VisionSegmentDbus>),
    Error(String),
    Done,
}

#[derive(Clone, Copy)]
enum VisionTextKind {
    Description,
    Ocr,
}

enum VisionPhase {
    CreatingSession,
    LoadingModel,
    Describing,
    Ocr,
    Segmenting,
}

impl VisionPhase {
    fn title(&self) -> &'static str {
        match self {
            VisionPhase::CreatingSession => "Creating vision session",
            VisionPhase::LoadingModel => "Loading vision model",
            VisionPhase::Describing => "Describing image",
            VisionPhase::Ocr => "Extracting text",
            VisionPhase::Segmenting => "Segmenting image",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            VisionPhase::CreatingSession => "Opening a vision session through the portal...",
            VisionPhase::LoadingModel => "Starting the local vision container if it is cold...",
            VisionPhase::Describing => "Sending image bytes to the vision model...",
            VisionPhase::Ocr => "Asking the vision model to extract text from the image...",
            VisionPhase::Segmenting => "Asking the vision model for normalized object boxes...",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Type)]
struct VisionSegmentDbus {
    label: String,
    confidence: f64,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

enum EmbedEvent {
    Phase(EmbedPhase),
    Embedding(Vec<f64>),
    Error(String),
    Done,
}

enum EmbedPhase {
    CreatingSession,
    LoadingModel,
    Embedding,
}

impl EmbedPhase {
    fn title(&self) -> &'static str {
        match self {
            EmbedPhase::CreatingSession => "Creating embedding session",
            EmbedPhase::LoadingModel => "Loading embedding model",
            EmbedPhase::Embedding => "Computing embedding",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            EmbedPhase::CreatingSession => "Opening a language.embed session through the portal...",
            EmbedPhase::LoadingModel => "Starting the local model container if it is cold...",
            EmbedPhase::Embedding => "Sending text to the model for embedding...",
        }
    }
}

fn temp_audio_path() -> PathBuf {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("aileron-demo-asr-{now}.f32le"))
}

fn friendly_error(error: &anyhow::Error) -> String {
    friendly_error_text(&error.to_string())
}

fn friendly_error_text(message: &str) -> String {
    if let Some(start) = message.find("reason: \"") {
        let rest = &message[start + "reason: \"".len()..];
        let mut out = String::new();
        let mut escaped = false;
        for ch in rest.chars() {
            if escaped {
                match ch {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    other => out.push(other),
                }
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                break;
            } else {
                out.push(ch);
            }
        }
        if !out.trim().is_empty() {
            return concise_error(&out);
        }
    }

    concise_error(message)
}

fn concise_error(message: &str) -> String {
    if message.contains("org.freedesktop.portal.Desktop")
        && message.contains("activation request failed: unknown unit")
    {
        return "xdg-desktop-portal is not available for D-Bus activation. Install and start the patched xdg-desktop-portal with the Aileron portal interfaces enabled.".to_string();
    }

    if message.contains("org.freedesktop.DBus.Error.UnknownInterface")
        && message.contains("org.freedesktop.portal.Language")
    {
        return "The running xdg-desktop-portal does not expose org.freedesktop.portal.Language. Rebuild and restart the patched xdg-desktop-portal, then ensure the Aileron implementation backend is configured.".to_string();
    }

    if message.contains("huggingface.co") && message.contains("ggml-") {
        return "Speech model is missing from the assigned container image. The container tried to download a Whisper model from Hugging Face, but Aileron starts inference containers with networking disabled. Rebuild or assign a Speech image that has the Whisper model baked into /model.".to_string();
    }

    message.to_string()
}

/// Call `StreamResponse` on the portal and forward token signals via `tx`.
fn summarize_streaming(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    // Separate connections for method calls and signal subscriptions —
    // the blocking zbus connection is single-threaded; mixing signals and
    // method calls on the same connection causes deadlocks.
    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;

    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_handle = create_public_session(
        &proxy,
        DemoMode::Summarize.use_case(),
        DemoMode::Summarize.instructions(),
    )?;

    // Subscribe to ModelLoading before generation, so no signals are missed
    // during the model load.
    let mut loading_iter = sig_proxy.receive_signal("ModelLoading")?;
    let loading_session_handle = session_handle.clone();
    let tx_loading = tx.clone();
    std::thread::spawn(move || {
        for msg in &mut loading_iter {
            if let Ok((_sig_request, sig_session, message)) =
                msg.body()
                    .deserialize::<(OwnedObjectPath, OwnedObjectPath, String)>()
                && sig_session.as_str() == loading_session_handle.as_str()
            {
                let _ = tx_loading.send(DemoEvent::Status(message));
                break;
            }
        }
    });

    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;

    let input_json = text_shorthand_json(&DemoMode::Summarize.prompt(text));

    // Subscribe before generation and consume concurrently. The D-Bus method
    // reply only marks stream completion; tokens are delivered as signals.
    let mut token_iter = sig_proxy.receive_signal("TokenReceived")?;
    let token_session_handle = session_handle.clone();
    let tx_tokens = tx.clone();
    let (stream_done_tx, stream_done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            for msg in &mut token_iter {
                let body = msg.body();
                let (_sig_request, sig_session, token, done): (
                    OwnedObjectPath,
                    OwnedObjectPath,
                    String,
                    bool,
                ) = body.deserialize()?;
                if sig_session.as_str() != token_session_handle.as_str() {
                    continue;
                }
                tx_tokens.send(DemoEvent::Token(token))?;
                if done {
                    break;
                }
            }
            Ok(())
        })();
        let _ = stream_done_tx.send(result);
    });

    let options = generation_options(512, "", "");
    tx.send(DemoEvent::Phase(DemoPhase::RequestingStream))?;
    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        "StreamResponse",
        &(&session_handle, &input_json, Vec::<OwnedFd>::new(), options),
    );
    let request_handle = match stream_result {
        Ok(handle) => handle,
        Err(error) => {
            let _ = close_public_session(&session_handle);
            return Err(error.into());
        }
    };
    stream_done_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| anyhow::anyhow!("stream completed without a final TokenReceived signal"))??;
    wait_request_success(&request_handle)?;

    close_public_session(&session_handle)?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}

fn stream_language_text(
    session_handle: &OwnedObjectPath,
    prompt: &str,
    options: PortalOptions,
    token_tx: Option<std::sync::mpsc::Sender<DemoEvent>>,
) -> anyhow::Result<String> {
    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let mut token_iter = sig_proxy.receive_signal("TokenReceived")?;
    let token_session_handle = session_handle.clone();
    let (stream_done_tx, stream_done_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<String> {
            let mut content = String::new();
            for msg in &mut token_iter {
                let (_sig_request, sig_session, token, done): (
                    OwnedObjectPath,
                    OwnedObjectPath,
                    String,
                    bool,
                ) = msg.body().deserialize()?;
                if sig_session.as_str() != token_session_handle.as_str() {
                    continue;
                }
                content.push_str(&token);
                if let Some(tx) = &token_tx {
                    tx.send(DemoEvent::Token(token))?;
                }
                if done {
                    break;
                }
            }
            Ok(content)
        })();
        let _ = stream_done_tx.send(result);
    });

    let input_json = text_shorthand_json(prompt);
    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        "StreamResponse",
        &(session_handle, &input_json, Vec::<OwnedFd>::new(), options),
    );
    let request_handle = stream_result?;
    let content = stream_done_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| anyhow::anyhow!("stream completed without a final TokenReceived signal"))??;
    wait_request_success(&request_handle)?;
    Ok(content)
}

enum GuidedStreamEvent {
    Snapshot(String, bool),
    ToolCalls(Vec<ToolCallDbus>, bool),
}

type SnapshotHandler<'a> = &'a mut dyn FnMut(&str) -> anyhow::Result<()>;

fn stream_guided_response(
    session_handle: &OwnedObjectPath,
    prompt: &str,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    stream_guided_call(None, session_handle, prompt, fields, tools, options, None)
}

fn stream_guided_response_with_snapshots(
    session_handle: &OwnedObjectPath,
    prompt: &str,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
    snapshot_tx: std::sync::mpsc::Sender<DemoEvent>,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    let mut send_snapshot = move |snapshot: &str| {
        snapshot_tx.send(DemoEvent::Json(snapshot.to_string()))?;
        Ok(())
    };

    stream_guided_response_with_snapshot_handler(
        session_handle,
        prompt,
        fields,
        tools,
        options,
        &mut send_snapshot,
    )
}

fn stream_guided_response_with_snapshot_handler(
    session_handle: &OwnedObjectPath,
    prompt: &str,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
    snapshot_handler: &mut dyn FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    stream_guided_call(
        None,
        session_handle,
        prompt,
        fields,
        tools,
        options,
        Some(snapshot_handler),
    )
}

fn stream_guided_tool_results(
    session_handle: &OwnedObjectPath,
    prompt: &str,
    results: Vec<ToolResultDbus>,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    stream_guided_call(
        Some(results),
        session_handle,
        prompt,
        fields,
        tools,
        options,
        None,
    )
}

fn stream_guided_call(
    results: Option<Vec<ToolResultDbus>>,
    session_handle: &OwnedObjectPath,
    prompt: &str,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
    mut snapshot_handler: Option<SnapshotHandler<'_>>,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let signal_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let mut signal_iter = signal_proxy.receive_all_signals()?;
    let signal_session_handle = session_handle.clone();
    let (event_tx, event_rx) = std::sync::mpsc::channel::<anyhow::Result<GuidedStreamEvent>>();

    std::thread::spawn(move || {
        for msg in &mut signal_iter {
            let result = (|| -> anyhow::Result<Option<GuidedStreamEvent>> {
                let header = msg.header();
                let member = header
                    .member()
                    .map(|member| member.as_str())
                    .unwrap_or_default();
                match member {
                    "GuidedSnapshotReceived" => {
                        let (_sig_request, sig_session, snapshot, done): (
                            OwnedObjectPath,
                            OwnedObjectPath,
                            String,
                            bool,
                        ) = msg.body().deserialize()?;
                        if sig_session.as_str() != signal_session_handle.as_str() {
                            return Ok(None);
                        }
                        Ok(Some(GuidedStreamEvent::Snapshot(snapshot, done)))
                    }
                    "GuidedToolCallsReceived" => {
                        let (_sig_request, sig_session, tool_calls, done): (
                            OwnedObjectPath,
                            OwnedObjectPath,
                            Vec<ToolCallDbus>,
                            bool,
                        ) = msg.body().deserialize()?;
                        if sig_session.as_str() != signal_session_handle.as_str() {
                            return Ok(None);
                        }
                        Ok(Some(GuidedStreamEvent::ToolCalls(tool_calls, done)))
                    }
                    _ => Ok(None),
                }
            })();
            match result {
                Ok(Some(event)) => {
                    let done = matches!(
                        &event,
                        GuidedStreamEvent::Snapshot(_, true)
                            | GuidedStreamEvent::ToolCalls(_, true)
                    );
                    let _ = event_tx.send(Ok(event));
                    if done {
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = event_tx.send(Err(error));
                    break;
                }
            }
        }
    });

    let request_handle: OwnedObjectPath = if let Some(results) = results {
        proxy.call(
            "StreamSubmitToolResultsGuided",
            &(session_handle, prompt, results, fields, tools, options),
        )?
    } else {
        proxy.call(
            "StreamRespondGuided",
            &(session_handle, prompt, fields, tools, options),
        )?
    };

    loop {
        match event_rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| anyhow::anyhow!("stream completed without a final guided signal"))??
        {
            GuidedStreamEvent::Snapshot(snapshot, done) => {
                if let Some(handler) = snapshot_handler.as_mut() {
                    handler(&snapshot)?;
                }
                if done {
                    wait_request_success(&request_handle)?;
                    return Ok((snapshot, Vec::new()));
                }
            }
            GuidedStreamEvent::ToolCalls(tool_calls, done) => {
                if done {
                    wait_request_success(&request_handle)?;
                    return Ok((String::new(), tool_calls));
                }
            }
        }
    }
}

fn stream_prediction(
    session_handle: &OwnedObjectPath,
    prefix: &str,
    options: PortalOptions,
) -> anyhow::Result<Vec<String>> {
    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let mut prediction_iter = sig_proxy.receive_signal("PredictionReceived")?;
    let prediction_session_handle = session_handle.clone();
    let (stream_done_tx, stream_done_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<Vec<String>> {
            let mut completions = Vec::new();
            for msg in &mut prediction_iter {
                let (_sig_request, sig_session, completion, done): (
                    OwnedObjectPath,
                    OwnedObjectPath,
                    String,
                    bool,
                ) = msg.body().deserialize()?;
                if sig_session.as_str() != prediction_session_handle.as_str() {
                    continue;
                }
                if !completion.is_empty() {
                    completions.push(completion);
                }
                if done {
                    return Ok(completions);
                }
            }
            Ok(Vec::new())
        })();
        let _ = stream_done_tx.send(result);
    });

    let stream_result: zbus::Result<OwnedObjectPath> =
        proxy.call("StreamPredictNext", &(session_handle, prefix, options));
    let request_handle = stream_result?;
    let completions = stream_done_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| {
            anyhow::anyhow!("stream completed without a final PredictionReceived signal")
        })??;
    wait_request_success(&request_handle)?;
    Ok(completions)
}

fn stream_embedding(session_handle: &OwnedObjectPath, text: &str) -> anyhow::Result<Vec<f64>> {
    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let mut embedding_iter = sig_proxy.receive_signal("EmbeddingReceived")?;
    let embedding_session_handle = session_handle.clone();
    let (stream_done_tx, stream_done_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<Vec<f64>> {
            for msg in &mut embedding_iter {
                let (_sig_request, sig_session, embedding, done): (
                    OwnedObjectPath,
                    OwnedObjectPath,
                    Vec<f64>,
                    bool,
                ) = msg.body().deserialize()?;
                if sig_session.as_str() != embedding_session_handle.as_str() {
                    continue;
                }
                if done {
                    return Ok(embedding);
                }
            }
            Ok(Vec::new())
        })();
        let _ = stream_done_tx.send(result);
    });

    let stream_result: zbus::Result<OwnedObjectPath> =
        proxy.call("StreamEmbed", &(session_handle, text, empty_options()));
    let request_handle = stream_result?;
    let embedding = stream_done_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| {
            anyhow::anyhow!("stream completed without a final EmbeddingReceived signal")
        })??;
    wait_request_success(&request_handle)?;
    Ok(embedding)
}

fn stream_vision_text(
    session_handle: &OwnedObjectPath,
    image: &[u8],
    instructions: &str,
    method: &str,
    text_tx: Option<(std::sync::mpsc::Sender<VisionEvent>, VisionTextKind)>,
) -> anyhow::Result<String> {
    let image_file = media_file_from_bytes(image)?;
    let image_fd = Fd::from(&image_file);
    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;
    let mut text_iter = sig_proxy.receive_signal("VisionTextReceived")?;
    let text_session_handle = session_handle.clone();
    let (stream_done_tx, stream_done_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<String> {
            let mut content = String::new();
            for msg in &mut text_iter {
                let (_sig_request, sig_session, text, done): (
                    OwnedObjectPath,
                    OwnedObjectPath,
                    String,
                    bool,
                ) = msg.body().deserialize()?;
                if sig_session.as_str() != text_session_handle.as_str() {
                    continue;
                }
                content.push_str(&text);
                if let Some((tx, kind)) = &text_tx {
                    let event = match kind {
                        VisionTextKind::Description => VisionEvent::Description(content.clone()),
                        VisionTextKind::Ocr => VisionEvent::Ocr(content.clone()),
                    };
                    tx.send(event)?;
                }
                if done {
                    break;
                }
            }
            Ok(content)
        })();
        let _ = stream_done_tx.send(result);
    });

    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        method,
        &(session_handle, image_fd, instructions, empty_options()),
    );
    let request_handle = stream_result?;
    let content = stream_done_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| {
            anyhow::anyhow!("stream completed without a final VisionTextReceived signal")
        })??;
    wait_request_success(&request_handle)?;
    Ok(content)
}

fn stream_vision_segments(
    session_handle: &OwnedObjectPath,
    image: &[u8],
    instructions: &str,
) -> anyhow::Result<Vec<VisionSegmentDbus>> {
    let image_file = media_file_from_bytes(image)?;
    let image_fd = Fd::from(&image_file);
    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;
    let mut segment_iter = sig_proxy.receive_signal("VisionSegmentsReceived")?;
    let segment_session_handle = session_handle.clone();
    let (stream_done_tx, stream_done_rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<Vec<VisionSegmentDbus>> {
            for msg in &mut segment_iter {
                let (_sig_request, sig_session, segments, done): (
                    OwnedObjectPath,
                    OwnedObjectPath,
                    Vec<VisionSegmentDbus>,
                    bool,
                ) = msg.body().deserialize()?;
                if sig_session.as_str() != segment_session_handle.as_str() {
                    continue;
                }
                if done {
                    return Ok(segments);
                }
            }
            Ok(Vec::new())
        })();
        let _ = stream_done_tx.send(result);
    });

    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        "StreamSegment",
        &(session_handle, image_fd, instructions, empty_options()),
    );
    let request_handle = stream_result?;
    let segments = stream_done_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| {
            anyhow::anyhow!("stream completed without a final VisionSegmentsReceived signal")
        })??;
    wait_request_success(&request_handle)?;
    Ok(segments)
}

fn extract_guided(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_handle = create_public_session(
        &proxy,
        DemoMode::Extract.use_case(),
        DemoMode::Extract.instructions(),
    )?;

    let prompt = DemoMode::Extract.prompt(text);
    let fields = vec![
        (
            "summary".to_string(),
            "string".to_string(),
            "A concise one-paragraph summary".to_string(),
            true,
        ),
        (
            "key_points".to_string(),
            "string_array".to_string(),
            "Three to five important points from the article".to_string(),
            true,
        ),
        (
            "confidence".to_string(),
            "integer".to_string(),
            "Confidence score from 0 to 100".to_string(),
            true,
        ),
    ];
    let options = generation_options(128, "", "");

    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;
    tx.send(DemoEvent::Phase(DemoPhase::RequestingGuided))?;
    let (content, _) = stream_guided_response_with_snapshots(
        &session_handle,
        &prompt,
        fields,
        Vec::<ToolDefinitionDbus>::new(),
        options,
        tx.clone(),
    )?;
    let pretty = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or(content);
    tx.send(DemoEvent::Json(pretty))?;

    close_public_session(&session_handle)?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}

fn classify_guided(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_handle = create_public_session(
        &proxy,
        DemoMode::Classify.use_case(),
        DemoMode::Classify.instructions(),
    )?;

    let fields = vec![
        (
            "topic".to_string(),
            "string".to_string(),
            "A concise topic label for the text".to_string(),
            true,
        ),
        (
            "intent".to_string(),
            "string".to_string(),
            "The likely intent, such as news, opinion, request, warning, or promotion".to_string(),
            true,
        ),
        (
            "rationale".to_string(),
            "string".to_string(),
            "One sentence explaining why the labels fit".to_string(),
            true,
        ),
        (
            "confidence".to_string(),
            "integer".to_string(),
            "Confidence score from 0 to 100".to_string(),
            true,
        ),
    ];
    let options = generation_options(512, "", "");

    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;
    tx.send(DemoEvent::Phase(DemoPhase::RequestingGuided))?;
    let (content, _) = stream_guided_response_with_snapshots(
        &session_handle,
        &DemoMode::Classify.prompt(text),
        fields,
        Vec::<ToolDefinitionDbus>::new(),
        options,
        tx.clone(),
    )?;
    let pretty = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or(content);
    tx.send(DemoEvent::Json(pretty))?;

    close_public_session(&session_handle)?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}

fn respond_text_task(
    mode: DemoMode,
    text: &str,
    tx: std::sync::mpsc::Sender<DemoEvent>,
) -> anyhow::Result<()> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_handle = create_public_session(&proxy, mode.use_case(), mode.instructions())?;

    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;
    tx.send(DemoEvent::Phase(DemoPhase::RequestingResponse))?;
    let options = match mode {
        DemoMode::Translate => generation_options(512, "", "Spanish"),
        _ => generation_options(512, "", ""),
    };
    let content = stream_language_text(
        &session_handle,
        &mode.prompt(text),
        options,
        Some(tx.clone()),
    )?;
    tx.send(DemoEvent::Text(content))?;

    close_public_session(&session_handle)?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}

fn predict_inline_completion(
    existing_session: Option<String>,
    input: &str,
) -> anyhow::Result<(String, Vec<String>)> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let used_existing_session = existing_session.is_some();
    let create_session = || -> anyhow::Result<OwnedObjectPath> {
        let handle = create_public_session(
            &proxy,
            "language.complete",
            "Inline typing prediction session.",
        )?;
        let request_handle: OwnedObjectPath = proxy.call("Prewarm", &(&handle, empty_options()))?;
        wait_request_success(&request_handle)?;
        Ok(handle)
    };
    let mut session_handle = match existing_session {
        Some(id) => cached_session_handle(id)?,
        None => create_session()?,
    };

    let prompt_input = if input.chars().count() > 2048 {
        input
            .chars()
            .rev()
            .take(2048)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>()
    } else {
        input.to_string()
    };
    let options = generation_options(4, "", "");
    let completions_result = stream_prediction(&session_handle, &prompt_input, options);
    let completions = match completions_result {
        Ok(completions) => completions,
        Err(e) if used_existing_session && is_session_not_found_message(&e.to_string()) => {
            session_handle = create_session()?;
            match stream_prediction(
                &session_handle,
                &prompt_input,
                generation_options(4, "", ""),
            ) {
                Ok(completions) => completions,
                Err(e) => {
                    let _ = close_public_session(&session_handle);
                    return Err(e);
                }
            }
        }
        Err(e) => return Err(e),
    };
    let mut cleaned = Vec::new();
    for completion in completions {
        let completion = clean_prediction(input, &completion);
        if !completion.trim().is_empty() && !cleaned.contains(&completion) {
            cleaned.push(completion);
        }
        if cleaned.len() == 3 {
            break;
        }
    }
    Ok((session_handle.to_string(), cleaned))
}

fn clear_failed_prediction_session(
    current: &mut Option<String>,
    attempted_session: Option<&str>,
) -> Option<String> {
    let attempted_session = attempted_session?;
    if current.as_deref() == Some(attempted_session) {
        current.take()
    } else {
        None
    }
}

fn is_session_not_found_message(message: &str) -> bool {
    message.contains("aileron.Inference.SessionNotFound")
        || message.contains("aileron.Inference.SessionNotFound_Args")
        || message.contains("SessionNotFound_Args")
}

fn end_prediction_session(session_id: &str) -> anyhow::Result<()> {
    close_public_session(&cached_session_handle(session_id.to_string())?)?;
    Ok(())
}

fn clean_prediction(input: &str, raw: &str) -> String {
    let mut suggestion = raw
        .trim_end()
        .trim_matches(['"', '\'', '`'])
        .replace(['\r', '\n'], " ");
    while suggestion.contains("  ") {
        suggestion = suggestion.replace("  ", " ");
    }

    if let Some(stripped) = suggestion.strip_prefix(input) {
        suggestion = stripped.trim_start().to_string();
    }
    if suggestion.to_ascii_lowercase().starts_with("continuation:") {
        suggestion = suggestion["continuation:".len()..].trim_start().to_string();
    }
    if suggestion.to_ascii_lowercase().starts_with("completion:") {
        suggestion = suggestion["completion:".len()..].trim_start().to_string();
    }

    let starts_with_boundary = suggestion.chars().next().is_some_and(char::is_whitespace);
    let suffix_mode = !starts_with_boundary
        && input
            .chars()
            .last()
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    suggestion = one_prediction_unit(&suggestion, suffix_mode);

    let mut out = String::new();
    for ch in suggestion.chars() {
        if out.chars().count() >= 96 {
            break;
        }
        out.push(ch);
    }

    if out.is_empty() {
        return out;
    }
    let input_ends_with_space = input.chars().last().is_some_and(char::is_whitespace);
    let out_starts_with_space = out.chars().next().is_some_and(char::is_whitespace);
    if !suffix_mode
        && !input_ends_with_space
        && !out_starts_with_space
        && !out.starts_with(['.', ',', ';', ':', '!', '?'])
    {
        out.insert(0, ' ');
    }
    out
}

fn one_prediction_unit(raw: &str, suffix_mode: bool) -> String {
    let trimmed = raw.trim_start();
    let mut out = String::new();
    let mut started = false;

    for ch in trimmed.chars() {
        let is_word = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '\'';
        if is_word {
            started = true;
            out.push(ch);
        } else if suffix_mode && !started {
            continue;
        } else {
            break;
        }
    }

    out
}

fn guided_chat_turn(
    existing_session: Option<String>,
    memory: &[String],
    messages: Vec<ChatMessage>,
    tx: std::sync::mpsc::Sender<ChatEvent>,
) -> anyhow::Result<()> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let used_existing_session = existing_session.is_some();

    let create_session = || -> anyhow::Result<OwnedObjectPath> {
        let handle = create_public_session(
            &proxy,
            "language.extract",
            "You answer chat turns and extract only durable user memory as guided JSON.",
        )?;
        tx.send(ChatEvent::SessionReady(handle.to_string()))?;
        Ok(handle)
    };

    let mut session_handle = match existing_session {
        Some(id) => cached_session_handle(id)?,
        None => create_session()?,
    };

    let fields = vec![
        (
            "answer".to_string(),
            "string".to_string(),
            "A concise, helpful answer to the user's latest message".to_string(),
            true,
        ),
        (
            "memory".to_string(),
            "string".to_string(),
            "One durable fact or preference to remember for future turns, or an empty string if there is nothing worth remembering".to_string(),
            true,
        ),
        (
            "confidence".to_string(),
            "integer".to_string(),
            "Confidence score from 0 to 100 for the answer".to_string(),
            true,
        ),
    ];
    let options = generation_options(512, "", "");
    let prompt = guided_chat_prompt(memory, &messages);
    let mut send_draft = |snapshot: &str| {
        if let Some(answer) = guided_chat_answer_draft(snapshot) {
            tx.send(ChatEvent::Draft(answer))?;
        }
        Ok(())
    };
    let response_result = stream_guided_response_with_snapshot_handler(
        &session_handle,
        &prompt,
        fields.clone(),
        Vec::<ToolDefinitionDbus>::new(),
        options,
        &mut send_draft,
    );
    let (content, _) = match response_result {
        Ok(response) => response,
        Err(e) if used_existing_session && is_session_not_found_message(&e.to_string()) => {
            session_handle = create_session()?;
            match stream_guided_response_with_snapshot_handler(
                &session_handle,
                &prompt,
                fields,
                Vec::<ToolDefinitionDbus>::new(),
                generation_options(512, "", ""),
                &mut send_draft,
            ) {
                Ok(response) => response,
                Err(e) => {
                    let _ = close_public_session(&session_handle);
                    return Err(e);
                }
            }
        }
        Err(e) => return Err(e),
    };
    let response: GuidedChatResponse = serde_json::from_str(&content)?;
    tx.send(ChatEvent::Response(response))?;

    tx.send(ChatEvent::Done)?;
    Ok(())
}

fn end_guided_chat_session(session_id: &str) -> anyhow::Result<()> {
    close_public_session(&cached_session_handle(session_id.to_string())?)?;
    Ok(())
}

fn guided_chat_prompt(memory: &[String], messages: &[ChatMessage]) -> String {
    let mut prompt = String::from(
        "Answer the latest user message using the conversation and memory below. Return only the guided JSON fields. Add memory only for durable user facts, preferences, goals, or constraints that would be useful in later turns; otherwise return an empty memory string.\n\nMemory:\n",
    );

    if memory.is_empty() {
        prompt.push_str("- None\n");
    } else {
        for item in memory.iter().rev().take(12).rev() {
            prompt.push_str("- ");
            prompt.push_str(item);
            prompt.push('\n');
        }
    }

    prompt.push_str("\nConversation:\n");
    for message in messages.iter().rev().take(12).rev() {
        prompt.push_str(&message.role);
        prompt.push_str(": ");
        prompt.push_str(&message.content);
        prompt.push('\n');
    }

    prompt
}

fn guided_chat_answer_draft(snapshot: &str) -> Option<String> {
    let answer = serde_json::from_str::<serde_json::Value>(snapshot)
        .ok()
        .and_then(|value| {
            value
                .get("answer")
                .and_then(|answer| answer.as_str())
                .map(str::to_string)
        })
        .or_else(|| partial_json_string_field(snapshot, "answer"))?;
    let answer = answer.trim().to_string();
    if answer.is_empty() {
        None
    } else {
        Some(answer)
    }
}

fn partial_json_string_field(input: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let field_start = input.find(&needle)?;
    let after_field = &input[field_start + needle.len()..];
    let colon = after_field.find(':')?;
    let after_colon = after_field[colon + 1..].trim_start();
    let mut chars = after_colon.chars();
    if chars.next()? != '"' {
        return None;
    }

    let mut value = String::new();
    let mut escaped = false;
    for ch in chars {
        if escaped {
            match ch {
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                '"' => value.push('"'),
                '\\' => value.push('\\'),
                '/' => value.push('/'),
                other => value.push(other),
            }
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            break;
        } else {
            value.push(ch);
        }
    }

    Some(value)
}

#[derive(Clone, Copy)]
enum SpeechTranscriptMode {
    Replace,
    Append,
}

fn speech_instructions(use_case: &str) -> &'static str {
    if use_case == "speech.translate" {
        "Translate the provided audio into English accurately."
    } else {
        "Transcribe the provided audio accurately."
    }
}

fn transcribe_recording(
    path: &PathBuf,
    use_case: &str,
    source_language_hint: &str,
    tx: std::sync::mpsc::Sender<SpeechEvent>,
) -> anyhow::Result<()> {
    let audio = std::fs::read(path)?;
    if audio.is_empty() {
        anyhow::bail!("recording is empty");
    }

    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, SPEECH_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, SPEECH_IFACE)?;

    tx.send(SpeechEvent::Phase(SpeechPhase::CreatingSession))?;
    let session_handle = create_public_session(&proxy, use_case, speech_instructions(use_case))?;

    let result: anyhow::Result<()> = (|| {
        tx.send(SpeechEvent::Phase(SpeechPhase::LoadingModel))?;
        tx.send(SpeechEvent::Phase(SpeechPhase::Transcribing))?;
        stream_speech_audio(
            &proxy,
            &sig_proxy,
            &session_handle,
            &audio,
            source_language_hint,
            &tx,
            SpeechTranscriptMode::Replace,
        )?;
        Ok(())
    })();

    end_speech_session(&session_handle);
    result?;
    tx.send(SpeechEvent::Done)?;
    Ok(())
}

fn live_transcribe_recording(
    path: PathBuf,
    use_case: &str,
    source_language_hint: &str,
    stop: Arc<AtomicBool>,
    tx: std::sync::mpsc::Sender<SpeechEvent>,
) -> anyhow::Result<()> {
    let source_language_hint = source_language_hint.to_string();
    let call_conn = portal_connection()?;
    let signal_conn = zbus::blocking::Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, SPEECH_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, SPEECH_IFACE)?;

    tx.send(SpeechEvent::Phase(SpeechPhase::CreatingSession))?;
    let session_handle = create_public_session(&proxy, use_case, speech_instructions(use_case))?;

    let result: anyhow::Result<()> = (|| {
        tx.send(SpeechEvent::Phase(SpeechPhase::LiveRecording))?;
        let mut offset = 0_u64;
        while !stop.load(Ordering::Acquire) {
            let len = match std::fs::metadata(&path) {
                Ok(metadata) => metadata.len(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::thread::sleep(std::time::Duration::from_millis(LIVE_SPEECH_POLL_MS));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            let aligned_len = len - (len % 4);
            if aligned_len.saturating_sub(offset) >= LIVE_SPEECH_MIN_CHUNK_BYTES {
                let chunk = read_audio_range(&path, offset, aligned_len)?;
                offset = aligned_len;
                if !chunk.is_empty() {
                    tx.send(SpeechEvent::Phase(SpeechPhase::LiveChunk))?;
                    stream_speech_audio(
                        &proxy,
                        &sig_proxy,
                        &session_handle,
                        &chunk,
                        &source_language_hint,
                        &tx,
                        SpeechTranscriptMode::Append,
                    )?;
                    tx.send(SpeechEvent::Phase(SpeechPhase::LiveRecording))?;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(LIVE_SPEECH_POLL_MS));
        }

        tx.send(SpeechEvent::Phase(SpeechPhase::Finalizing))?;
        let audio = std::fs::read(&path)?;
        if audio.is_empty() {
            anyhow::bail!("recording is empty");
        }
        tx.send(SpeechEvent::Transcript(String::new()))?;
        stream_speech_audio(
            &proxy,
            &sig_proxy,
            &session_handle,
            &audio,
            &source_language_hint,
            &tx,
            SpeechTranscriptMode::Replace,
        )?;
        Ok(())
    })();

    end_speech_session(&session_handle);
    result?;
    tx.send(SpeechEvent::Done)?;
    Ok(())
}

fn stream_speech_audio(
    proxy: &zbus::blocking::Proxy<'_>,
    sig_proxy: &zbus::blocking::Proxy<'_>,
    session_handle: &OwnedObjectPath,
    audio: &[u8],
    source_language_hint: &str,
    tx: &std::sync::mpsc::Sender<SpeechEvent>,
    mode: SpeechTranscriptMode,
) -> anyhow::Result<String> {
    let audio_file = media_file_from_bytes(audio)?;
    let audio_fd = Fd::from(&audio_file);
    let mut transcription_iter = sig_proxy.receive_signal("TranscriptionReceived")?;
    let transcript_session_handle = session_handle.clone();
    let tx_transcript = tx.clone();
    let (stream_done_tx, stream_done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<String> {
            let mut transcript = String::new();
            for msg in &mut transcription_iter {
                let (_sig_request, sig_session, text, done): (
                    OwnedObjectPath,
                    OwnedObjectPath,
                    String,
                    bool,
                ) = msg.body().deserialize()?;
                if sig_session.as_str() != transcript_session_handle.as_str() {
                    continue;
                }
                transcript.push_str(&text);
                match mode {
                    SpeechTranscriptMode::Replace => {
                        tx_transcript.send(SpeechEvent::Transcript(transcript.clone()))?;
                    }
                    SpeechTranscriptMode::Append => {
                        if !text.is_empty() {
                            tx_transcript.send(SpeechEvent::AppendTranscript(text))?;
                        }
                    }
                }
                if done {
                    break;
                }
            }
            Ok(transcript)
        })();
        let _ = stream_done_tx.send(result);
    });

    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        "StreamTranscribe",
        &(
            session_handle,
            audio_fd,
            source_language_hint,
            empty_options(),
        ),
    );
    let request_handle = stream_result?;
    let transcript = stream_done_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| {
            anyhow::anyhow!("stream completed without a final TranscriptionReceived signal")
        })??;
    wait_request_success(&request_handle)?;
    Ok(transcript)
}

fn read_audio_range(path: &Path, start: u64, end: u64) -> anyhow::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    let len = end.saturating_sub(start);
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = vec![0_u8; len as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn end_speech_session(session_handle: &OwnedObjectPath) {
    let _ = close_public_session(session_handle);
}

fn describe_image(
    image: &[u8],
    instructions: &str,
    tx: std::sync::mpsc::Sender<VisionEvent>,
) -> anyhow::Result<()> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;

    tx.send(VisionEvent::Phase(VisionPhase::CreatingSession))?;
    let session_handle = create_public_session(
        &proxy,
        "vision.describe",
        "Describe the provided image clearly and concisely.",
    )?;

    tx.send(VisionEvent::Phase(VisionPhase::LoadingModel))?;
    tx.send(VisionEvent::Phase(VisionPhase::Describing))?;
    let description = stream_vision_text(
        &session_handle,
        image,
        instructions,
        "StreamDescribe",
        Some((tx.clone(), VisionTextKind::Description)),
    )?;
    tx.send(VisionEvent::Description(description))?;

    close_public_session(&session_handle)?;
    tx.send(VisionEvent::Done)?;
    Ok(())
}

fn ocr_image(
    image: &[u8],
    instructions: &str,
    tx: std::sync::mpsc::Sender<VisionEvent>,
) -> anyhow::Result<()> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;

    tx.send(VisionEvent::Phase(VisionPhase::CreatingSession))?;
    let session_handle = create_public_session(
        &proxy,
        "vision.ocr",
        "Extract all text visible in the provided image exactly as written.",
    )?;

    tx.send(VisionEvent::Phase(VisionPhase::LoadingModel))?;
    tx.send(VisionEvent::Phase(VisionPhase::Ocr))?;
    let text = stream_vision_text(
        &session_handle,
        image,
        instructions,
        "StreamOcr",
        Some((tx.clone(), VisionTextKind::Ocr)),
    )?;
    tx.send(VisionEvent::Ocr(text))?;

    close_public_session(&session_handle)?;
    tx.send(VisionEvent::Done)?;
    Ok(())
}

fn segment_image(
    image: &[u8],
    instructions: &str,
    tx: std::sync::mpsc::Sender<VisionEvent>,
) -> anyhow::Result<()> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;

    tx.send(VisionEvent::Phase(VisionPhase::CreatingSession))?;
    let session_handle = create_public_session(
        &proxy,
        "vision.segment",
        "Identify visible objects and return normalized rectangular boxes.",
    )?;

    tx.send(VisionEvent::Phase(VisionPhase::LoadingModel))?;
    tx.send(VisionEvent::Phase(VisionPhase::Segmenting))?;
    let segments = stream_vision_segments(&session_handle, image, instructions)?;
    tx.send(VisionEvent::Segments(segments))?;

    close_public_session(&session_handle)?;
    tx.send(VisionEvent::Done)?;
    Ok(())
}

fn embed_text(text: &str, tx: std::sync::mpsc::Sender<EmbedEvent>) -> anyhow::Result<()> {
    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;

    tx.send(EmbedEvent::Phase(EmbedPhase::CreatingSession))?;
    let session_handle = create_public_session(
        &proxy,
        "language.embed",
        "Compute an embedding vector for the provided text.",
    )?;

    tx.send(EmbedEvent::Phase(EmbedPhase::LoadingModel))?;
    tx.send(EmbedEvent::Phase(EmbedPhase::Embedding))?;
    let embedding = stream_embedding(&session_handle, text)?;
    tx.send(EmbedEvent::Embedding(embedding))?;

    close_public_session(&session_handle)?;
    tx.send(EmbedEvent::Done)?;
    Ok(())
}

fn format_segments(segments: &[VisionSegmentDbus]) -> String {
    if segments.is_empty() {
        return "No objects returned.".to_string();
    }

    segments
        .iter()
        .enumerate()
        .map(|(idx, segment)| {
            format!(
                "{}. {} ({:.0}%) x={:.3}, y={:.3}, w={:.3}, h={:.3}",
                idx + 1,
                segment.label,
                segment.confidence * 100.0,
                segment.x,
                segment.y,
                segment.width,
                segment.height
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_embedding(vector: &[f64]) -> String {
    if vector.is_empty() {
        return "Model returned an empty embedding.".to_string();
    }

    let magnitude = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
    let preview_len = vector.len().min(16);
    let preview = vector[..preview_len]
        .iter()
        .map(|v| format!("{v:+.4}"))
        .collect::<Vec<_>>()
        .join(", ");
    let ellipsis = if vector.len() > preview_len {
        ", ..."
    } else {
        ""
    };

    format!(
        "dimensions: {}\nL2 norm: {:.4}\n\n[{}{}]",
        vector.len(),
        magnitude,
        preview,
        ellipsis
    )
}

fn media_file_from_bytes(bytes: &[u8]) -> anyhow::Result<std::fs::File> {
    use std::io::{Seek, SeekFrom, Write};
    use std::os::fd::FromRawFd;
    use std::os::raw::{c_char, c_int, c_uint};

    const MFD_ALLOW_SEALING: c_uint = 0x0002;
    const F_ADD_SEALS: c_int = 1033;
    const F_SEAL_SHRINK: c_int = 0x0002;
    const F_SEAL_GROW: c_int = 0x0004;
    const F_SEAL_WRITE: c_int = 0x0008;

    unsafe extern "C" {
        fn memfd_create(name: *const c_char, flags: c_uint) -> c_int;
        fn fcntl(fd: c_int, cmd: c_int, arg: c_int) -> c_int;
    }

    let name = std::ffi::CString::new("aileron-demo-media")?;
    let fd = unsafe { memfd_create(name.as_ptr(), MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;

    let seals = F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_SHRINK;
    if unsafe { fcntl(fd, F_ADD_SEALS, seals) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    Ok(file)
}

pub(crate) fn decode_base64(input: &str) -> Result<Vec<u8>, String> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut table = [255u8; 256];
    for (i, &b) in alphabet.iter().enumerate() {
        table[b as usize] = i as u8;
    }

    let clean = input
        .bytes()
        .filter(|b| !matches!(b, b'=' | b'\n' | b'\r' | b' ' | b'\t'))
        .collect::<Vec<_>>();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;

    for b in clean {
        let v = table[b as usize];
        if v == 255 {
            return Err(format!("invalid base64 char: {}", b as char));
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{
        DemoMode, clean_prediction, clear_failed_prediction_session, concise_error,
        guided_chat_answer_draft, is_session_not_found_message,
    };
    use hegel::TestCase;
    use hegel::generators as gs;

    #[test]
    fn explains_missing_portal_systemd_unit() {
        let error = "org.freedesktop.DBus.Error.NameHasNoOwner: Could not activate remote peer 'org.freedesktop.portal.Desktop': activation request failed: unknown unit";

        assert_eq!(
            concise_error(error),
            "xdg-desktop-portal is not available for D-Bus activation. Install and start the patched xdg-desktop-portal with the Aileron portal interfaces enabled."
        );
    }

    #[test]
    fn explains_stale_portal_language_interface() {
        let error = "org.freedesktop.DBus.Error.UnknownInterface: Unknown interface 'org.freedesktop.portal.Language'";

        assert_eq!(
            concise_error(error),
            "The running xdg-desktop-portal does not expose org.freedesktop.portal.Language. Rebuild and restart the patched xdg-desktop-portal, then ensure the Aileron implementation backend is configured."
        );
    }

    #[test]
    fn failed_prediction_clears_matching_cached_session() {
        let mut current = Some("session-a".to_string());

        let cleared = clear_failed_prediction_session(&mut current, Some("session-a"));

        assert_eq!(cleared.as_deref(), Some("session-a"));
        assert_eq!(current, None);
    }

    #[test]
    fn failed_prediction_keeps_unrelated_cached_session() {
        let mut current = Some("session-b".to_string());

        let cleared = clear_failed_prediction_session(&mut current, Some("session-a"));

        assert_eq!(cleared, None);
        assert_eq!(current.as_deref(), Some("session-b"));
    }

    #[test]
    fn stale_prediction_session_errors_are_detected() {
        let error = "org.freedesktop.DBus.Error.Failed: aileron.Inference.SessionNotFound: Some(SessionNotFound_Args { session_id: \"missing\" })";

        assert!(is_session_not_found_message(error));
        assert!(is_session_not_found_message(
            "org.freedesktop.DBus.Error.Failed: GDBus.Error:org.freedesktop.DBus.Error.Failed: aileron.Inference.SessionNotFound: Some(SessionNotFound_Args { session_id: \"missing\" })"
        ));
        assert!(!is_session_not_found_message(
            "aileron.Inference.GenerationFailed"
        ));
    }

    #[test]
    fn clean_prediction_preserves_next_word_boundary() {
        assert_eq!(clean_prediction("hey, das ist", " eine"), " eine");
        assert_eq!(clean_prediction("hey, my", " 10-year"), " 10-year");
    }

    #[test]
    fn clean_prediction_keeps_current_word_suffixes_attached() {
        assert_eq!(clean_prediction("runn", "ing"), "ing");
    }

    #[test]
    fn guided_chat_answer_draft_reads_complete_snapshot() {
        assert_eq!(
            guided_chat_answer_draft(r#"{"answer":"Streaming now","memory":"","confidence":88}"#)
                .as_deref(),
            Some("Streaming now")
        );
    }

    #[test]
    fn guided_chat_answer_draft_reads_partial_snapshot() {
        assert_eq!(
            guided_chat_answer_draft(r#"{"answer":"Streaming in fl"#).as_deref(),
            Some("Streaming in fl")
        );
    }

    #[hegel::test]
    fn demo_prompts_include_generated_input_prefix(tc: TestCase) {
        let mode = match tc.draw(gs::integers::<u8>().max_value(5)) {
            0 => DemoMode::Summarize,
            1 => DemoMode::Translate,
            2 => DemoMode::Rephrase,
            3 => DemoMode::Classify,
            4 => DemoMode::Extract,
            _ => DemoMode::Analyze,
        };
        let text = tc.draw(gs::sampled_from(vec![
            "short input".to_string(),
            "local models should stay private".to_string(),
            "Aileron routes tasks by use-case".to_string(),
        ]));

        assert!(mode.prompt(&text).contains(&text));
    }
}
