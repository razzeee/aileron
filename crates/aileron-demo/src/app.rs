/// aileron-demo — sandboxed GTK4 article summarizer.
mod frontends;
pub(crate) mod tool_demo;

use gtk4::prelude::*;
use gtk4::{Button, DropDown, Label};
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
use std::time::{SystemTime, UNIX_EPOCH};
use zbus::zvariant::{Fd, OwnedFd, OwnedObjectPath, OwnedValue, Type, Value};

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const LANGUAGE_IFACE: &str = "org.freedesktop.portal.Language";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";
const SESSION_IFACE: &str = "org.freedesktop.portal.Session";
const SPEECH_IFACE: &str = "org.freedesktop.portal.Speech";
const VISION_IFACE: &str = "org.freedesktop.portal.Vision";
static PORTAL_CONNECTION: OnceLock<zbus::blocking::Connection> = OnceLock::new();
static USE_BACKGROUND_EXECUTION: AtomicBool = AtomicBool::new(false);

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

fn selected_execution_mode() -> &'static str {
    if USE_BACKGROUND_EXECUTION.load(Ordering::Relaxed) {
        "background"
    } else {
        "interactive"
    }
}

fn execution_options() -> PortalOptions {
    let mut options = HashMap::new();
    options.insert(
        "execution_mode".to_string(),
        string_option_value(selected_execution_mode()),
    );
    options
}

fn generation_options(
    maximum_response_tokens: i64,
    source_language_hint: &str,
    target_language_hint: &str,
) -> PortalOptions {
    let mut options = execution_options();
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

fn speech_options(source_language_hint: &str) -> PortalOptions {
    let mut options = execution_options();
    options.insert(
        "source_language_hint".to_string(),
        string_option_value(source_language_hint),
    );
    options
}

fn create_public_session(
    proxy: &zbus::blocking::Proxy<'_>,
    use_case: &str,
    instructions: &str,
) -> anyhow::Result<OwnedObjectPath> {
    let conn = portal_connection()?;
    let token_suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let handle_token = format!("create_{token_suffix}");
    let session_handle_token = format!("session_{token_suffix}");
    let request_path = portal_request_path(&conn, &handle_token)?;
    let request_proxy =
        zbus::blocking::Proxy::new(&conn, PORTAL_BUS, request_path.as_str(), REQUEST_IFACE)?;
    let mut response_iter = request_proxy.receive_signal("Response")?;
    let mut options = empty_options();
    options.insert(
        "handle_token".to_string(),
        string_option_value(&handle_token),
    );
    options.insert(
        "session_handle_token".to_string(),
        string_option_value(&session_handle_token),
    );

    let request_handle: OwnedObjectPath =
        proxy.call("CreateSession", &("", use_case, instructions, options))?;
    let mut results = wait_request_response_from_iter(&mut response_iter)?;
    let session_handle = results
        .remove("session_handle")
        .ok_or_else(|| anyhow::anyhow!("CreateSession response omitted session_handle"))?;
    let session_handle = OwnedObjectPath::try_from(session_handle)
        .map_err(|e| anyhow::anyhow!("CreateSession returned invalid session handle: {e}"))?;
    if request_handle.as_str() != request_path {
        anyhow::bail!(
            "CreateSession returned unexpected request handle: {}",
            request_handle.as_str()
        );
    }
    Ok(session_handle)
}

fn portal_request_path(conn: &zbus::blocking::Connection, token: &str) -> anyhow::Result<String> {
    let unique = conn
        .unique_name()
        .ok_or_else(|| anyhow::anyhow!("portal connection has no unique bus name"))?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    Ok(format!("{PORTAL_PATH}/request/{unique}/{token}"))
}

fn wait_request_response(
    request_handle: &OwnedObjectPath,
) -> anyhow::Result<HashMap<String, OwnedValue>> {
    let conn = portal_connection()?;
    let proxy =
        zbus::blocking::Proxy::new(&conn, PORTAL_BUS, request_handle.as_str(), REQUEST_IFACE)?;
    let mut response_iter = proxy.receive_signal("Response")?;
    wait_request_response_from_iter(&mut response_iter)
}

fn wait_request_response_from_iter<I>(
    response_iter: &mut I,
) -> anyhow::Result<HashMap<String, OwnedValue>>
where
    I: Iterator<Item = zbus::Message>,
{
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
    overview_page.set_icon_name(Some("view-grid-symbolic"));
    let text_page = stack.add_titled(&frontends::text::build_page(), Some("text"), "Text lab");
    text_page.set_icon_name(Some("text-x-generic-symbolic"));
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
    let execution_mode_dropdown = DropDown::from_strings(&["Interactive", "Background"]);
    execution_mode_dropdown.set_tooltip_text(Some("Execution mode sent with portal requests"));
    execution_mode_dropdown.set_selected(if USE_BACKGROUND_EXECUTION.load(Ordering::Relaxed) {
        1
    } else {
        0
    });
    execution_mode_dropdown.connect_selected_notify(|dropdown| {
        USE_BACKGROUND_EXECUTION.store(dropdown.selected() == 1, Ordering::Relaxed);
    });
    content_header.pack_end(&execution_mode_dropdown);
    let title = WindowTitle::builder()
        .title("Lab overview")
        .subtitle("Aileron demo")
        .build();
    let title_for_stack = title.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        title_for_stack.set_title(match stack.visible_child_name().as_deref() {
            Some("overview") => "Lab overview",
            Some("text") => "Text lab",
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
                "Return only a valid JSON object with summary, key_points, and confidence. Do not include markdown, commentary, or prose outside the JSON. Summarize this article as structured data. Keep the summary short, include 3-5 key points, and set confidence from 0 to 100:\n\n{trimmed}"
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
    let call_conn = portal_connection()?;
    let signal_conn = call_conn.clone();

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
    let (stream_event_tx, stream_event_rx) = std::sync::mpsc::channel();
    let token_event_tx = stream_event_tx.clone();
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
                token_event_tx.send(Ok(TokenStreamEvent::Token(token, done)))?;
                if done {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = token_event_tx.send(Err(error));
        }
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
    let response_request_handle = request_handle.clone();
    std::thread::spawn(move || {
        let result = wait_request_success(&response_request_handle);
        let _ = stream_event_tx.send(Ok(TokenStreamEvent::RequestDone(result)));
    });

    let mut terminal_seen = false;
    let mut request_done = false;
    loop {
        match stream_event_rx.recv() {
            Ok(event) => match event? {
                TokenStreamEvent::Token(token, done) => {
                    tx_tokens.send(DemoEvent::Token(token))?;
                    if done {
                        terminal_seen = true;
                    }
                }
                TokenStreamEvent::RequestDone(result) => {
                    result?;
                    request_done = true;
                }
            },
            Err(std::sync::mpsc::RecvError) => {
                anyhow::bail!("token stream ended before the request completed");
            }
        }
        if terminal_seen && request_done {
            break;
        }
    }

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
    let signal_conn = call_conn.clone();
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let mut token_iter = sig_proxy.receive_signal("TokenReceived")?;
    let token_session_handle = session_handle.clone();
    let (stream_event_tx, stream_event_rx) = std::sync::mpsc::channel();
    let token_event_tx = stream_event_tx.clone();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
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
                token_event_tx.send(Ok(TokenStreamEvent::Token(token, done)))?;
                if done {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = token_event_tx.send(Err(error));
        }
    });

    let input_json = text_shorthand_json(prompt);
    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        "StreamResponse",
        &(session_handle, &input_json, Vec::<OwnedFd>::new(), options),
    );
    let request_handle = stream_result?;
    let response_request_handle = request_handle.clone();
    std::thread::spawn(move || {
        let result = wait_request_success(&response_request_handle);
        let _ = stream_event_tx.send(Ok(TokenStreamEvent::RequestDone(result)));
    });

    let mut content = String::new();
    let mut terminal_content = None::<String>;
    let mut request_done = false;
    loop {
        match stream_event_rx.recv() {
            Ok(event) => match event? {
                TokenStreamEvent::Token(token, done) => {
                    content.push_str(&token);
                    if let Some(tx) = &token_tx {
                        tx.send(DemoEvent::Token(token))?;
                    }
                    if done {
                        terminal_content = Some(content.clone());
                    }
                }
                TokenStreamEvent::RequestDone(result) => {
                    result?;
                    request_done = true;
                }
            },
            Err(std::sync::mpsc::RecvError) => {
                anyhow::bail!("token stream ended before the request completed");
            }
        }
        if request_done && let Some(content) = terminal_content.take() {
            return Ok(content);
        }
    }
}

enum TokenStreamEvent {
    Token(String, bool),
    RequestDone(anyhow::Result<()>),
}

enum GuidedStreamEvent {
    Snapshot(String, bool),
    ToolCalls(Vec<ToolCallDbus>, bool),
    RequestDone(anyhow::Result<()>),
}

type SnapshotHandler<'a> = &'a mut dyn FnMut(&str) -> anyhow::Result<()>;

fn stream_guided_response(
    session_handle: &OwnedObjectPath,
    prompt: &str,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    stream_guided_call(GuidedPortalCall {
        results: None,
        session_handle,
        prompt,
        media_files: Vec::new(),
        fields,
        tools,
        options,
        snapshot_handler: None,
    })
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
    stream_guided_call(GuidedPortalCall {
        results: None,
        session_handle,
        prompt,
        media_files: Vec::new(),
        fields,
        tools,
        options,
        snapshot_handler: Some(snapshot_handler),
    })
}

fn stream_guided_tool_results(
    session_handle: &OwnedObjectPath,
    prompt: &str,
    results: Vec<ToolResultDbus>,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    stream_guided_call(GuidedPortalCall {
        results: Some(results),
        session_handle,
        prompt,
        media_files: Vec::new(),
        fields,
        tools,
        options,
        snapshot_handler: None,
    })
}

fn stream_guided_response_with_media_and_snapshot_handler(
    session_handle: &OwnedObjectPath,
    prompt: &str,
    media_files: Vec<std::fs::File>,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
    snapshot_handler: &mut dyn FnMut(&str) -> anyhow::Result<()>,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    stream_guided_call(GuidedPortalCall {
        results: None,
        session_handle,
        prompt,
        media_files,
        fields,
        tools,
        options,
        snapshot_handler: Some(snapshot_handler),
    })
}

struct GuidedPortalCall<'a> {
    results: Option<Vec<ToolResultDbus>>,
    session_handle: &'a OwnedObjectPath,
    prompt: &'a str,
    media_files: Vec<std::fs::File>,
    fields: Vec<(String, String, String, bool)>,
    tools: Vec<ToolDefinitionDbus>,
    options: PortalOptions,
    snapshot_handler: Option<SnapshotHandler<'a>>,
}

fn stream_guided_call(
    request: GuidedPortalCall<'_>,
) -> anyhow::Result<(String, Vec<ToolCallDbus>)> {
    let GuidedPortalCall {
        results,
        session_handle,
        prompt,
        media_files,
        fields,
        tools,
        options,
        mut snapshot_handler,
    } = request;
    let call_conn = portal_connection()?;
    let signal_conn = call_conn.clone();
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let signal_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let mut signal_iter = signal_proxy.receive_all_signals()?;
    let signal_session_handle = session_handle.clone();
    let (event_tx, event_rx) = std::sync::mpsc::channel::<anyhow::Result<GuidedStreamEvent>>();
    let media_fds = media_files.iter().map(Fd::from).collect::<Vec<_>>();
    let signal_tx = event_tx.clone();

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
                    let _ = signal_tx.send(Ok(event));
                    if done {
                        break;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = signal_tx.send(Err(error));
                    break;
                }
            }
        }
    });

    let request_handle: OwnedObjectPath = if let Some(results) = results {
        proxy.call(
            "StreamSubmitToolResultsGuided",
            &(
                session_handle,
                prompt,
                &media_fds,
                results,
                fields,
                tools,
                options,
            ),
        )?
    } else {
        proxy.call(
            "StreamRespondGuided",
            &(session_handle, prompt, &media_fds, fields, tools, options),
        )?
    };

    let response_tx = event_tx.clone();
    let response_request_handle = request_handle.clone();
    std::thread::spawn(move || {
        let result = wait_request_success(&response_request_handle);
        let _ = response_tx.send(Ok(GuidedStreamEvent::RequestDone(result)));
    });

    let mut terminal_response = None::<(String, Vec<ToolCallDbus>)>;
    let mut request_done = false;
    loop {
        match event_rx.recv() {
            Ok(event) => match event? {
                GuidedStreamEvent::Snapshot(snapshot, done) => {
                    if let Some(handler) = snapshot_handler.as_mut() {
                        handler(&snapshot)?;
                    }
                    if done {
                        terminal_response = Some((snapshot, Vec::new()));
                    }
                }
                GuidedStreamEvent::ToolCalls(tool_calls, done) => {
                    if done {
                        terminal_response = Some((String::new(), tool_calls));
                    }
                }
                GuidedStreamEvent::RequestDone(result) => {
                    result?;
                    request_done = true;
                }
            },
            Err(std::sync::mpsc::RecvError) => {
                anyhow::bail!("guided stream ended before a final guided signal");
            }
        }
        if request_done && let Some(response) = terminal_response.take() {
            return Ok(response);
        }
    }
}

fn stream_embedding(session_handle: &OwnedObjectPath, text: &str) -> anyhow::Result<Vec<f64>> {
    let call_conn = portal_connection()?;
    let signal_conn = call_conn.clone();
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let mut embedding_iter = sig_proxy.receive_signal("EmbeddingReceived")?;
    let embedding_session_handle = session_handle.clone();
    let (stream_event_tx, stream_event_rx) = std::sync::mpsc::channel();
    let embedding_event_tx = stream_event_tx.clone();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
            for msg in &mut embedding_iter {
                let (_sig_request, sig_session, embedding, _embedding_pipeline_id, done): (
                    OwnedObjectPath,
                    OwnedObjectPath,
                    Vec<f64>,
                    String,
                    bool,
                ) = msg.body().deserialize()?;
                if sig_session.as_str() != embedding_session_handle.as_str() {
                    continue;
                }
                embedding_event_tx.send(Ok(EmbeddingStreamEvent::Embedding(embedding, done)))?;
                if done {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = embedding_event_tx.send(Err(error));
        }
    });

    let stream_result: zbus::Result<OwnedObjectPath> =
        proxy.call("StreamEmbed", &(session_handle, text, execution_options()));
    let request_handle = stream_result?;
    let response_request_handle = request_handle.clone();
    std::thread::spawn(move || {
        let result = wait_request_success(&response_request_handle);
        let _ = stream_event_tx.send(Ok(EmbeddingStreamEvent::RequestDone(result)));
    });

    let mut terminal_embedding = None::<Vec<f64>>;
    let mut request_done = false;
    loop {
        match stream_event_rx.recv() {
            Ok(event) => match event? {
                EmbeddingStreamEvent::Embedding(value, done) => {
                    if done {
                        terminal_embedding = Some(value);
                    }
                }
                EmbeddingStreamEvent::RequestDone(result) => {
                    result?;
                    request_done = true;
                }
            },
            Err(std::sync::mpsc::RecvError) => {
                anyhow::bail!("embedding stream ended before the request completed");
            }
        }
        if request_done && let Some(embedding) = terminal_embedding.take() {
            return Ok(embedding);
        }
    }
}

enum EmbeddingStreamEvent {
    Embedding(Vec<f64>, bool),
    RequestDone(anyhow::Result<()>),
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
    let signal_conn = call_conn.clone();
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;
    let mut text_iter = sig_proxy.receive_signal("VisionTextReceived")?;
    let text_session_handle = session_handle.clone();
    let (stream_event_tx, stream_event_rx) = std::sync::mpsc::channel();
    let text_event_tx = stream_event_tx.clone();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
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
                text_event_tx.send(Ok(VisionTextStreamEvent::Text(text, done)))?;
                if done {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = text_event_tx.send(Err(error));
        }
    });

    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        method,
        &(session_handle, image_fd, instructions, execution_options()),
    );
    let request_handle = stream_result?;
    let response_request_handle = request_handle.clone();
    std::thread::spawn(move || {
        let result = wait_request_success(&response_request_handle);
        let _ = stream_event_tx.send(Ok(VisionTextStreamEvent::RequestDone(result)));
    });

    let mut content = String::new();
    let mut terminal_content = None::<String>;
    let mut request_done = false;
    loop {
        match stream_event_rx.recv() {
            Ok(event) => match event? {
                VisionTextStreamEvent::Text(text, done) => {
                    content.push_str(&text);
                    if let Some((tx, kind)) = &text_tx {
                        let event = match kind {
                            VisionTextKind::Description => {
                                VisionEvent::Description(content.clone())
                            }
                            VisionTextKind::Ocr => VisionEvent::Ocr(content.clone()),
                        };
                        tx.send(event)?;
                    }
                    if done {
                        terminal_content = Some(content.clone());
                    }
                }
                VisionTextStreamEvent::RequestDone(result) => {
                    result?;
                    request_done = true;
                }
            },
            Err(std::sync::mpsc::RecvError) => {
                anyhow::bail!("vision text stream ended before the request completed");
            }
        }
        if request_done && let Some(content) = terminal_content.take() {
            return Ok(content);
        }
    }
}

enum VisionTextStreamEvent {
    Text(String, bool),
    RequestDone(anyhow::Result<()>),
}

fn stream_vision_segments(
    session_handle: &OwnedObjectPath,
    image: &[u8],
    instructions: &str,
) -> anyhow::Result<Vec<VisionSegmentDbus>> {
    let image_file = media_file_from_bytes(image)?;
    let image_fd = Fd::from(&image_file);
    let call_conn = portal_connection()?;
    let signal_conn = call_conn.clone();
    let proxy = zbus::blocking::Proxy::new(&call_conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;
    let sig_proxy =
        zbus::blocking::Proxy::new(&signal_conn, PORTAL_BUS, PORTAL_PATH, VISION_IFACE)?;
    let mut segment_iter = sig_proxy.receive_signal("VisionSegmentsReceived")?;
    let segment_session_handle = session_handle.clone();
    let (stream_event_tx, stream_event_rx) = std::sync::mpsc::channel();
    let segment_event_tx = stream_event_tx.clone();

    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
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
                segment_event_tx.send(Ok(VisionSegmentStreamEvent::Segments(segments, done)))?;
                if done {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = segment_event_tx.send(Err(error));
        }
    });

    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        "StreamSegment",
        &(session_handle, image_fd, instructions, execution_options()),
    );
    let request_handle = stream_result?;
    let response_request_handle = request_handle.clone();
    std::thread::spawn(move || {
        let result = wait_request_success(&response_request_handle);
        let _ = stream_event_tx.send(Ok(VisionSegmentStreamEvent::RequestDone(result)));
    });

    let mut terminal_segments = None::<Vec<VisionSegmentDbus>>;
    let mut request_done = false;
    loop {
        match stream_event_rx.recv() {
            Ok(event) => match event? {
                VisionSegmentStreamEvent::Segments(value, done) => {
                    if done {
                        terminal_segments = Some(value);
                    }
                }
                VisionSegmentStreamEvent::RequestDone(result) => {
                    result?;
                    request_done = true;
                }
            },
            Err(std::sync::mpsc::RecvError) => {
                anyhow::bail!("vision segment stream ended before the request completed");
            }
        }
        if request_done && let Some(segments) = terminal_segments.take() {
            return Ok(segments);
        }
    }
}

enum VisionSegmentStreamEvent {
    Segments(Vec<VisionSegmentDbus>, bool),
    RequestDone(anyhow::Result<()>),
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
    let options = generation_options(512, "", "");

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

fn is_session_not_found_message(message: &str) -> bool {
    message.contains("aileron.Inference.SessionNotFound")
        || message.contains("aileron.Inference.SessionNotFound_Args")
        || message.contains("SessionNotFound_Args")
}

fn guided_chat_turn(
    existing_session: Option<String>,
    memory: &[String],
    messages: Vec<ChatMessage>,
    image: Option<Vec<u8>>,
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
    let text_prompt = guided_chat_prompt(memory, &messages);
    let image_bytes = image;
    let image_mime_type = image_bytes
        .as_deref()
        .map(image_mime_type)
        .unwrap_or("image/png");
    let prompt = if image_bytes.is_some() {
        serde_json::json!([
            { "type": "input_text", "text": text_prompt },
            { "type": "input_image", "fd_index": 0, "mime_type": image_mime_type }
        ])
        .to_string()
    } else {
        text_prompt
    };
    let media_files = chat_media_files(image_bytes.as_deref())?;
    let mut send_draft = |snapshot: &str| {
        if let Some(answer) = guided_chat_answer_draft(snapshot) {
            tx.send(ChatEvent::Draft(answer))?;
        }
        Ok(())
    };
    let response_result = stream_guided_response_with_media_and_snapshot_handler(
        &session_handle,
        &prompt,
        media_files,
        fields.clone(),
        Vec::<ToolDefinitionDbus>::new(),
        options,
        &mut send_draft,
    );
    let (content, _) = match response_result {
        Ok(response) => response,
        Err(e) if used_existing_session && is_session_not_found_message(&e.to_string()) => {
            session_handle = create_session()?;
            match stream_guided_response_with_media_and_snapshot_handler(
                &session_handle,
                &prompt,
                chat_media_files(image_bytes.as_deref())?,
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

fn chat_media_files(image: Option<&[u8]>) -> anyhow::Result<Vec<std::fs::File>> {
    image
        .map(media_file_from_bytes)
        .transpose()
        .map(|file| file.into_iter().collect())
}

fn image_mime_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else {
        "image/png"
    }
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
    let signal_conn = call_conn.clone();
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
    let signal_conn = call_conn.clone();
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
    let (stream_event_tx, stream_event_rx) = std::sync::mpsc::channel();
    let transcript_event_tx = stream_event_tx.clone();
    std::thread::spawn(move || {
        let result = (|| -> anyhow::Result<()> {
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
                transcript_event_tx.send(Ok(SpeechStreamEvent::Text(text, done)))?;
                if done {
                    break;
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = transcript_event_tx.send(Err(error));
        }
    });

    let stream_result: zbus::Result<OwnedObjectPath> = proxy.call(
        "StreamTranscribe",
        &(
            session_handle,
            audio_fd,
            speech_options(source_language_hint),
        ),
    );
    let request_handle = stream_result?;
    let response_request_handle = request_handle.clone();
    std::thread::spawn(move || {
        let result = wait_request_success(&response_request_handle);
        let _ = stream_event_tx.send(Ok(SpeechStreamEvent::RequestDone(result)));
    });

    let mut transcript = String::new();
    let mut terminal_transcript = None::<String>;
    let mut request_done = false;
    loop {
        match stream_event_rx.recv() {
            Ok(event) => match event? {
                SpeechStreamEvent::Text(text, done) => {
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
                        terminal_transcript = Some(transcript.clone());
                    }
                }
                SpeechStreamEvent::RequestDone(result) => {
                    result?;
                    request_done = true;
                }
            },
            Err(std::sync::mpsc::RecvError) => {
                anyhow::bail!("speech stream ended before the request completed");
            }
        }
        if request_done && let Some(transcript) = terminal_transcript.take() {
            return Ok(transcript);
        }
    }
}

enum SpeechStreamEvent {
    Text(String, bool),
    RequestDone(anyhow::Result<()>),
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

#[cfg(test)]
mod tests {
    use super::{DemoMode, concise_error, guided_chat_answer_draft, is_session_not_found_message};
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
    fn stale_session_errors_are_detected() {
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
