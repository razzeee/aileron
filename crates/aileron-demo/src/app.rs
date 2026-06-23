/// aileron-demo — sandboxed GTK4 article summarizer.
mod frontends;

use gtk4::prelude::*;
use gtk4::{Button, Label};
use libadwaita::prelude::*;
use libadwaita::{
    ApplicationWindow, HeaderBar, OverlaySplitView, ToolbarView, ViewStack, ViewSwitcherSidebar,
    WindowTitle,
};
use relm4::{ComponentParts, ComponentSender, RelmApp, SimpleComponent};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use zbus::zvariant::Type;

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
                "The local model returned the task result through Respond."
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
    Error(String),
    Done,
}

enum ToolEvent {
    Trace(String),
    Final(String),
    Error(String),
    Done,
}

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

#[derive(Debug, Clone, Deserialize)]
struct GuidedToolLoopResponse {
    action: String,
    tool_name: String,
    word: String,
    character: String,
    #[serde(default)]
    answer: String,
}

enum SpeechPhase {
    CreatingSession,
    LoadingModel,
    Transcribing,
}

impl SpeechPhase {
    fn title(&self) -> &'static str {
        match self {
            SpeechPhase::CreatingSession => "Creating Speech session",
            SpeechPhase::LoadingModel => "Loading Speech model",
            SpeechPhase::Transcribing => "Processing audio",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            SpeechPhase::CreatingSession => "Opening a Speech session through the portal...",
            SpeechPhase::LoadingModel => "Starting the local Speech container if it is cold...",
            SpeechPhase::Transcribing => "Sending recorded microphone audio to the Speech model...",
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
    if message.contains("org.freedesktop.impl.portal.desktop.aileron")
        && message.contains("activation request failed: unknown unit")
    {
        return "Aileron portal is not installed for D-Bus activation. Install systemd/aileron-portal.service to ~/.config/systemd/user/, run `systemctl --user daemon-reload`, then start `systemctl --user enable --now aileron-portal`.".to_string();
    }

    if message.contains("org.freedesktop.DBus.Error.UnknownInterface")
        && message.contains("org.freedesktop.impl.portal.Language")
    {
        return "The running Aileron portal is older than this demo and does not expose the Language interface. Restart the updated portal with `systemctl --user restart aileron-portal`, or rebuild/reinstall the portal service if it was installed from an older binary.".to_string();
    }

    if message.contains("huggingface.co") && message.contains("ggml-") {
        return "Speech model is missing from the assigned container image. The container tried to download a Whisper model from Hugging Face, but Aileron starts inference containers with networking disabled. Rebuild or assign a Speech image that has the Whisper model baked into /model.".to_string();
    }

    message.to_string()
}

/// Call `StreamResponse` on the portal and forward tokens via `tx`.
/// Sends `Some(token)` for each token, then `None` when done.
fn summarize_streaming(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    // Separate connections for method calls and signal subscriptions —
    // the blocking zbus connection is single-threaded; mixing signals and
    // method calls on the same connection causes deadlocks.
    let call_conn = Connection::session()?;
    let signal_conn = Connection::session()?;

    let proxy = zbus::blocking::Proxy::new(&call_conn, BUS, PATH, IFACE)?;
    let sig_proxy = zbus::blocking::Proxy::new(&signal_conn, BUS, PATH, IFACE)?;

    // Subscribe to ModelLoading on the signal connection before generation, so
    // no signals are missed during the model load.
    let mut loading_iter = sig_proxy.receive_signal("ModelLoading")?;

    let tx_loading = tx.clone();
    let loading_thread = std::thread::spawn(move || {
        for msg in &mut loading_iter {
            if let Ok(body) = msg.body().deserialize::<(String,)>() {
                let _ = tx_loading.send(DemoEvent::Status(body.0));
            }
        }
    });

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            DemoMode::Summarize.use_case(),
            DemoMode::Summarize.instructions(),
        ),
    )?;

    drop(loading_thread);
    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;

    let prompt = DemoMode::Summarize.prompt(text);

    // Subscribe to TokenReceived on the signal connection.
    let mut token_iter = sig_proxy.receive_signal("TokenReceived")?;

    // StreamResponse returns immediately; tokens arrive as D-Bus signals.
    let options = (512_i64, 0.7_f64, "default", "", "");
    tx.send(DemoEvent::Phase(DemoPhase::RequestingStream))?;
    let _: () = proxy.call("StreamResponse", &(&session_id, &prompt, options))?;

    for msg in &mut token_iter {
        let body = msg.body();
        let (sig_session, token, done): (String, String, bool) = body.deserialize()?;
        if sig_session != session_id {
            continue;
        }
        tx.send(DemoEvent::Token(token))?;
        if done {
            break;
        }
    }

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}

fn extract_guided(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            DemoMode::Extract.use_case(),
            DemoMode::Extract.instructions(),
        ),
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
    let options = (128_i64, 0.2_f64, "default", "", "");

    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;
    tx.send(DemoEvent::Phase(DemoPhase::RequestingGuided))?;
    let (content, _): (String, Vec<ToolCallDbus>) = proxy.call(
        "RespondGuided",
        &(
            &session_id,
            &prompt,
            fields,
            Vec::<ToolDefinitionDbus>::new(),
            options,
        ),
    )?;
    let pretty = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or(content);
    tx.send(DemoEvent::Json(pretty))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}

fn classify_guided(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            DemoMode::Classify.use_case(),
            DemoMode::Classify.instructions(),
        ),
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
    let options = (512_i64, 0.2_f64, "default", "", "");

    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;
    tx.send(DemoEvent::Phase(DemoPhase::RequestingGuided))?;
    let (content, _): (String, Vec<ToolCallDbus>) = proxy.call(
        "RespondGuided",
        &(
            &session_id,
            &DemoMode::Classify.prompt(text),
            fields,
            Vec::<ToolDefinitionDbus>::new(),
            options,
        ),
    )?;
    let pretty = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or(content);
    tx.send(DemoEvent::Json(pretty))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}

fn respond_text_task(
    mode: DemoMode,
    text: &str,
    tx: std::sync::mpsc::Sender<DemoEvent>,
) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &("org.aileron.Demo", mode.use_case(), mode.instructions()),
    )?;

    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;
    tx.send(DemoEvent::Phase(DemoPhase::RequestingResponse))?;
    let options = match mode {
        DemoMode::Translate => (512_i64, 0.3_f64, "default", "", "Spanish"),
        _ => (512_i64, 0.5_f64, "default", "", ""),
    };
    let content: String = proxy.call("Respond", &(&session_id, &mode.prompt(text), options))?;
    tx.send(DemoEvent::Text(content))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}

fn predict_inline_completion(
    existing_session: Option<String>,
    input: &str,
) -> anyhow::Result<(String, Vec<String>)> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;
    let used_existing_session = existing_session.is_some();
    let create_session = || -> anyhow::Result<String> {
        let id: String = proxy.call(
            "CreateSession",
            &(
                "org.aileron.Demo",
                "language.complete",
                "Inline typing prediction session.",
            ),
        )?;
        let _: () = proxy.call("Prewarm", &(&id, ""))?;
        Ok(id)
    };
    let mut session_id = match existing_session {
        Some(id) => id,
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
    let options = (4_i64, 0.0_f64, "greedy", "", "");
    let completions_result: zbus::Result<Vec<String>> =
        proxy.call("PredictNext", &(&session_id, &prompt_input, 3_i64, options));
    let completions = match completions_result {
        Ok(completions) => completions,
        Err(e) if used_existing_session && is_session_not_found_message(&e.to_string()) => {
            session_id = create_session()?;
            match proxy.call("PredictNext", &(&session_id, &prompt_input, 3_i64, options)) {
                Ok(completions) => completions,
                Err(e) => {
                    let _: zbus::Result<()> = proxy.call("EndSession", &(&session_id,));
                    return Err(e.into());
                }
            }
        }
        Err(e) => return Err(e.into()),
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
    Ok((session_id, cleaned))
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
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;
    let _: () = proxy.call("EndSession", &(session_id,))?;
    Ok(())
}

fn clean_prediction(input: &str, raw: &str) -> String {
    let mut suggestion = raw
        .trim()
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

    let suffix_mode = input
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
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    let session_id = match existing_session {
        Some(id) => id,
        None => {
            let id: String = proxy.call(
                "CreateSession",
                &(
                    "org.aileron.Demo",
                    "language.extract",
                    "You answer chat turns and extract only durable user memory as guided JSON.",
                ),
            )?;
            tx.send(ChatEvent::SessionReady(id.clone()))?;
            id
        }
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
    let options = (512_i64, 0.2_f64, "default", "", "");
    let prompt = guided_chat_prompt(memory, &messages);
    let (content, _): (String, Vec<ToolCallDbus>) = proxy.call(
        "RespondGuided",
        &(
            &session_id,
            &prompt,
            fields,
            Vec::<ToolDefinitionDbus>::new(),
            options,
        ),
    )?;
    let response: GuidedChatResponse = serde_json::from_str(&content)?;
    tx.send(ChatEvent::Response(response))?;

    tx.send(ChatEvent::Done)?;
    Ok(())
}

fn end_guided_chat_session(session_id: &str) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;
    let _: () = proxy.call("EndSession", &(session_id,))?;
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

fn run_character_tool_demo(
    prompt: &str,
    tx: std::sync::mpsc::Sender<ToolEvent>,
) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    tx.send(ToolEvent::Trace(
        "before_agent_loop: seed messages and register count_character_occurrences".to_string(),
    ))?;

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            "language.analyze",
            "You are a small local agent. Return guided JSON. Use action=call_tool when exact character counting is needed. Use action=final only after tool_result is provided.",
        ),
    )?;

    let fields = guided_tool_loop_fields();
    let tools = count_tool_definitions()?;
    let options = (128_i64, 0.2_f64, "default", "", "");
    let mut loop_prompt = format!(
        "Available app tool:\n- count_character_occurrences(word: string, character: string): exact deterministic count.\n\nUser request: {prompt}\n\nReturn action=call_tool with tool_name=count_character_occurrences, word, and character if this needs exact counting. Return action=final only if no tool is needed."
    );

    tx.send(ToolEvent::Trace(
        "before_llm_call: ask RespondGuided for app-loop action".to_string(),
    ))?;
    let (content, tool_calls): (String, Vec<ToolCallDbus>) = proxy.call(
        "RespondGuided",
        &(
            &session_id,
            &loop_prompt,
            fields.clone(),
            tools.clone(),
            options,
        ),
    )?;
    let mut response = if content.trim().is_empty() && !tool_calls.is_empty() {
        GuidedToolLoopResponse {
            action: "call_tool".to_string(),
            tool_name: "count_character_occurrences".to_string(),
            word: String::new(),
            character: String::new(),
            answer: String::new(),
        }
    } else {
        parse_guided_tool_loop_response(&content, prompt, false)?
    };
    tx.send(ToolEvent::Trace(format!(
        "after_llm_call: action={}, native_tool_calls={}, tool_name={}, word={}, character={}, answer={:?}",
        response.action,
        tool_calls.len(),
        response.tool_name,
        response.word,
        response.character,
        response.answer
    )))?;

    if response.action != "call_tool" && tool_calls.is_empty() {
        tx.send(ToolEvent::Trace(
            "after_agent_loop: guided response selected final answer without tool execution"
                .to_string(),
        ))?;
        let answer = match initial_final_answer(response) {
            Ok(answer) => answer,
            Err(e) => {
                let _: () = proxy.call("EndSession", &(&session_id,))?;
                return Err(e);
            }
        };
        tx.send(ToolEvent::Final(answer))?;
        let _: () = proxy.call("EndSession", &(&session_id,))?;
        tx.send(ToolEvent::Done)?;
        return Ok(());
    }

    let mut results = Vec::new();
    let result_json = if tool_calls.is_empty() {
        let args = serde_json::json!({
            "word": response.word,
            "character": response.character
        });
        tx.send(ToolEvent::Trace(format!(
            "before_tool_execution: count_character_occurrences args={args}"
        )))?;
        let result_json = execute_count_tool(prompt, &args.to_string())?;
        tx.send(ToolEvent::Trace(format!(
            "after_tool_execution: result={result_json}"
        )))?;
        result_json
    } else {
        let mut last_result = serde_json::Value::Null;
        for call in tool_calls {
            tx.send(ToolEvent::Trace(format!(
                "before_tool_execution: {} id={} args={}",
                call.name, call.id, call.arguments_json
            )))?;
            let result_json = execute_count_tool(prompt, &call.arguments_json)?;
            tx.send(ToolEvent::Trace(format!(
                "after_tool_execution: result={result_json}"
            )))?;
            results.push(ToolResultDbus {
                id: call.id,
                content: result_json.to_string(),
                content_json: result_json.to_string(),
            });
            last_result = result_json;
        }
        last_result
    };

    tx.send(ToolEvent::Trace(
        "before_llm_call: append tool_result to prompt and call RespondGuided again".to_string(),
    ))?;
    loop_prompt.push_str("\n\ntool_result from count_character_occurrences:\n");
    loop_prompt.push_str(&result_json.to_string());
    loop_prompt.push_str("\n\nNow return action=final and put the user-facing answer in answer.");
    let final_content = if results.is_empty() {
        let (content, _): (String, Vec<ToolCallDbus>) = proxy.call(
            "RespondGuided",
            &(&session_id, &loop_prompt, fields, tools, options),
        )?;
        content
    } else {
        let (content, _): (String, Vec<ToolCallDbus>) = proxy.call(
            "SubmitToolResultsGuided",
            &(&session_id, &loop_prompt, results, fields, tools, options),
        )?;
        content
    };
    response = parse_guided_tool_loop_response(&final_content, prompt, true)?;
    tx.send(ToolEvent::Trace(format!(
        "after_llm_call: action={}, answer={:?}",
        response.action, response.answer
    )))?;
    tx.send(ToolEvent::Trace(
        "after_agent_loop: stop after one guided tool round for this demo".to_string(),
    ))?;
    tx.send(ToolEvent::Final(
        if response.answer.trim().is_empty() || response.answer == "stub" {
            format_tool_result_answer(&result_json)
        } else {
            response.answer
        },
    ))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(ToolEvent::Done)?;
    Ok(())
}

fn execute_count_tool(prompt: &str, arguments_json: &str) -> anyhow::Result<serde_json::Value> {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments_json).unwrap_or_default();
    let word = parsed
        .get("word")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| infer_word_from_prompt(prompt))
        .unwrap_or_default();
    let character = parsed
        .get("character")
        .and_then(|value| value.as_str())
        .and_then(|value| value.chars().next())
        .or_else(|| infer_character_from_prompt(prompt))
        .unwrap_or('r');
    if word.is_empty() {
        anyhow::bail!("tool arguments did not include a word and the prompt did not contain one");
    }
    let count = word.chars().filter(|ch| *ch == character).count();
    Ok(serde_json::json!({
        "word": word,
        "character": character.to_string(),
        "count": count
    }))
}

fn guided_tool_loop_fields() -> Vec<(String, String, String, bool)> {
    vec![
        (
            "action".to_string(),
            "string".to_string(),
            "Either call_tool or final".to_string(),
            true,
        ),
        (
            "tool_name".to_string(),
            "string".to_string(),
            "Tool to execute, usually count_character_occurrences, or empty for final".to_string(),
            true,
        ),
        (
            "word".to_string(),
            "string".to_string(),
            "Word or text to pass to the tool".to_string(),
            true,
        ),
        (
            "character".to_string(),
            "string".to_string(),
            "Single character to pass to the tool".to_string(),
            true,
        ),
        (
            "answer".to_string(),
            "string".to_string(),
            "Final user-facing answer when action is final, otherwise empty".to_string(),
            false,
        ),
    ]
}

fn count_tool_definitions() -> anyhow::Result<Vec<ToolDefinitionDbus>> {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["word", "character"],
        "properties": {
            "word": {"type": "string", "description": "The word or short text to inspect"},
            "character": {"type": "string", "description": "The single character to count"}
        },
        "additionalProperties": false
    });

    Ok(vec![ToolDefinitionDbus {
        name: "count_character_occurrences".to_string(),
        description: "Count how many times one character appears in a word or short text."
            .to_string(),
        schema_json: serde_json::to_string(&schema)?,
    }])
}

fn parse_guided_tool_loop_response(
    content: &str,
    prompt: &str,
    expect_final: bool,
) -> anyhow::Result<GuidedToolLoopResponse> {
    let mut response = serde_json::from_str::<GuidedToolLoopResponse>(content)?;
    if expect_final {
        response.action = "final".to_string();
    }
    if response.action != "call_tool" && response.action != "final" {
        response.action = if content.contains("tool_result") {
            "final".to_string()
        } else {
            "call_tool".to_string()
        };
    }
    if response.tool_name.trim().is_empty() || response.tool_name == "stub" {
        response.tool_name = "count_character_occurrences".to_string();
    }
    if response.word.trim().is_empty() || response.word == "stub" {
        response.word = infer_word_from_prompt(prompt).unwrap_or_default();
    }
    if response.character.trim().is_empty() || response.character == "stub" {
        response.character = infer_character_from_prompt(prompt)
            .unwrap_or('r')
            .to_string();
    }
    Ok(response)
}

fn format_tool_result_answer(result: &serde_json::Value) -> String {
    let word = result["word"].as_str().unwrap_or("the input");
    let character = result["character"].as_str().unwrap_or("?");
    let count = result["count"].as_u64().unwrap_or_default();
    format!("The character '{character}' occurs {count} times in {word}.")
}

fn initial_final_answer(response: GuidedToolLoopResponse) -> anyhow::Result<String> {
    let answer = response.answer.trim().to_string();
    if answer.is_empty() || answer == "stub" {
        anyhow::bail!("guided response selected final without an answer");
    }
    Ok(answer)
}

fn infer_word_from_prompt(prompt: &str) -> Option<String> {
    let parts = prompt
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| part.len() > 1)
        .collect::<Vec<_>>();

    parts
        .iter()
        .enumerate()
        .rev()
        .find(|(_, part)| part.eq_ignore_ascii_case("in") || part.eq_ignore_ascii_case("within"))
        .and_then(|(index, _)| {
            parts[index + 1..]
                .iter()
                .rev()
                .find(|part| is_count_target_candidate(part))
        })
        .or_else(|| {
            parts
                .iter()
                .rev()
                .find(|part| is_count_target_candidate(part))
        })
        .map(|part| (*part).to_string())
}

fn is_count_target_candidate(part: &str) -> bool {
    let lower = part.to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "a" | "an"
            | "are"
            | "character"
            | "count"
            | "does"
            | "how"
            | "in"
            | "input"
            | "is"
            | "letter"
            | "many"
            | "occur"
            | "occurs"
            | "of"
            | "string"
            | "text"
            | "the"
            | "times"
            | "to"
            | "within"
            | "word"
    )
}

fn infer_character_from_prompt(prompt: &str) -> Option<char> {
    let lower = prompt.to_ascii_lowercase();
    lower
        .split("letter")
        .nth(1)
        .and_then(|rest| rest.chars().find(|ch| ch.is_ascii_alphabetic()))
        .or_else(|| {
            lower
                .split("character")
                .nth(1)
                .and_then(|rest| rest.chars().find(|ch| ch.is_ascii_alphabetic()))
        })
        .or_else(|| {
            lower
                .split(|ch: char| !ch.is_ascii_alphanumeric())
                .find_map(|part| {
                    let mut chars = part.chars();
                    let ch = chars.next()?;
                    (chars.next().is_none() && ch.is_ascii_alphabetic()).then_some(ch)
                })
        })
}

fn transcribe_recording(
    path: &PathBuf,
    use_case: &str,
    tx: std::sync::mpsc::Sender<SpeechEvent>,
) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Speech";

    let audio = std::fs::read(path)?;
    if audio.is_empty() {
        anyhow::bail!("recording is empty");
    }

    let instructions = if use_case == "speech.translate" {
        "Translate the provided audio into English accurately."
    } else {
        "Transcribe the provided audio accurately."
    };

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(SpeechEvent::Phase(SpeechPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &("org.aileron.Demo", use_case, instructions),
    )?;

    tx.send(SpeechEvent::Phase(SpeechPhase::LoadingModel))?;
    tx.send(SpeechEvent::Phase(SpeechPhase::Transcribing))?;
    let audio_b64 = base64_encode(&audio);
    // speech.translate reuses the Transcribe method; the daemon selects the
    // whisper transcribe-vs-translate task from the session use_case.
    let transcript: String = proxy.call("Transcribe", &(&session_id, &audio_b64, ""))?;
    tx.send(SpeechEvent::Transcript(transcript))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(SpeechEvent::Done)?;
    Ok(())
}

fn describe_image(image_b64: &str, tx: std::sync::mpsc::Sender<VisionEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Vision";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(VisionEvent::Phase(VisionPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            "vision.describe",
            "Describe the provided image clearly and concisely.",
        ),
    )?;

    tx.send(VisionEvent::Phase(VisionPhase::LoadingModel))?;
    tx.send(VisionEvent::Phase(VisionPhase::Describing))?;
    let description: String = proxy.call("Describe", &(&session_id, &image_b64))?;
    tx.send(VisionEvent::Description(description))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(VisionEvent::Done)?;
    Ok(())
}

fn ocr_image(image_b64: &str, tx: std::sync::mpsc::Sender<VisionEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Vision";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(VisionEvent::Phase(VisionPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            "vision.ocr",
            "Extract all text visible in the provided image exactly as written.",
        ),
    )?;

    tx.send(VisionEvent::Phase(VisionPhase::LoadingModel))?;
    tx.send(VisionEvent::Phase(VisionPhase::Ocr))?;
    let text: String = proxy.call("Ocr", &(&session_id, &image_b64))?;
    tx.send(VisionEvent::Ocr(text))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(VisionEvent::Done)?;
    Ok(())
}

fn segment_image(image_b64: &str, tx: std::sync::mpsc::Sender<VisionEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Vision";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(VisionEvent::Phase(VisionPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            "vision.segment",
            "Identify visible objects and return normalized rectangular boxes.",
        ),
    )?;

    tx.send(VisionEvent::Phase(VisionPhase::LoadingModel))?;
    tx.send(VisionEvent::Phase(VisionPhase::Segmenting))?;
    let segments: Vec<VisionSegmentDbus> = proxy.call("Segment", &(&session_id, &image_b64))?;
    tx.send(VisionEvent::Segments(segments))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(VisionEvent::Done)?;
    Ok(())
}

fn embed_text(text: &str, tx: std::sync::mpsc::Sender<EmbedEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.Language";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(EmbedEvent::Phase(EmbedPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            "language.embed",
            "Compute an embedding vector for the provided text.",
        ),
    )?;

    tx.send(EmbedEvent::Phase(EmbedPhase::LoadingModel))?;
    tx.send(EmbedEvent::Phase(EmbedPhase::Embedding))?;
    let embedding: Vec<f64> = proxy.call("Embed", &(&session_id, &text))?;
    tx.send(EmbedEvent::Embedding(embedding))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
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

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{
        DemoMode, base64_encode, clear_failed_prediction_session, concise_error,
        execute_count_tool, guided_tool_loop_fields, initial_final_answer,
        is_session_not_found_message, parse_guided_tool_loop_response,
    };
    use hegel::TestCase;
    use hegel::generators as gs;

    #[test]
    fn explains_missing_portal_systemd_unit() {
        let error = "org.freedesktop.DBus.Error.NameHasNoOwner: Could not activate remote peer 'org.freedesktop.impl.portal.desktop.aileron': activation request failed: unknown unit";

        assert_eq!(
            concise_error(error),
            "Aileron portal is not installed for D-Bus activation. Install systemd/aileron-portal.service to ~/.config/systemd/user/, run `systemctl --user daemon-reload`, then start `systemctl --user enable --now aileron-portal`."
        );
    }

    #[test]
    fn explains_stale_portal_language_interface() {
        let error = "org.freedesktop.DBus.Error.UnknownInterface: Unknown interface 'org.freedesktop.impl.portal.Language'";

        assert_eq!(
            concise_error(error),
            "The running Aileron portal is older than this demo and does not expose the Language interface. Restart the updated portal with `systemctl --user restart aileron-portal`, or rebuild/reinstall the portal service if it was installed from an older binary."
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
        assert!(!is_session_not_found_message(
            "aileron.Inference.GenerationFailed"
        ));
    }

    #[test]
    fn count_tool_uses_structured_arguments() {
        let result = execute_count_tool(
            "ignored prompt",
            r#"{"word":"strawrberrry","character":"r"}"#,
        )
        .expect("count tool should run");

        assert_eq!(result["count"], 5);
    }

    #[hegel::test]
    fn count_tool_counts_generated_structured_arguments(tc: TestCase) {
        let mut chars =
            tc.draw(gs::vecs(gs::sampled_from(vec!['a', 'b', 'e', 'r', 'z'])).max_size(32));
        if chars.is_empty() {
            chars.push('r');
        }
        let word = chars.iter().collect::<String>();
        let character = tc.draw(gs::sampled_from(vec!['a', 'b', 'e', 'r', 'z']));
        let arguments = serde_json::json!({
            "word": word,
            "character": character.to_string(),
        })
        .to_string();

        let result =
            execute_count_tool("ignored prompt", &arguments).expect("count tool should run");
        let expected = word.chars().filter(|ch| *ch == character).count() as u64;

        assert_eq!(result["word"], word);
        assert_eq!(result["character"], character.to_string());
        assert_eq!(result["count"].as_u64(), Some(expected));
    }

    #[test]
    fn count_tool_falls_back_to_prompt_for_stub_arguments() {
        let result = execute_count_tool(
            "How many times does the letter r occur in strawrberrry?",
            "{}",
        )
        .expect("count tool should infer demo args");

        assert_eq!(result["word"], "strawrberrry");
        assert_eq!(result["character"], "r");
        assert_eq!(result["count"], 5);
    }

    #[test]
    fn count_tool_infers_non_r_prompt_arguments() {
        let result = execute_count_tool(
            "How many times does the letter s occur in Mississippi?",
            "{}",
        )
        .expect("count tool should infer non-r demo args");

        assert_eq!(result["word"], "Mississippi");
        assert_eq!(result["character"], "s");
        assert_eq!(result["count"], 4);
    }

    #[hegel::test]
    fn guided_tool_loop_stub_fields_are_inferred_from_prompt(tc: TestCase) {
        let word = tc.draw(gs::sampled_from(vec![
            "strawberry".to_string(),
            "raspberry".to_string(),
            "cranberry".to_string(),
        ]));
        let character = tc.draw(gs::sampled_from(vec!['r', 'e', 'a']));
        let prompt = format!("How many times does the letter {character} occur in {word}?");
        let response = parse_guided_tool_loop_response(
            r#"{"action":"call_tool","tool_name":"stub","word":"stub","character":"stub","answer":""}"#,
            &prompt,
            false,
        )
        .expect("stub response should be repaired");

        assert_eq!(response.action, "call_tool");
        assert_eq!(response.tool_name, "count_character_occurrences");
        assert_eq!(response.word, word);
        assert_eq!(response.character, character.to_string());
    }

    #[test]
    fn guided_tool_loop_rejects_missing_text_tool_arguments() {
        let error = parse_guided_tool_loop_response(
            r#"{"action":"call_tool","tool_name":"count_character_occurrences","answer":""}"#,
            "How many times does the letter s occur in Mississippi?",
            false,
        )
        .expect_err("text tool calls should include structured arguments");

        assert!(error.to_string().contains("missing field"));
    }

    #[test]
    fn guided_tool_loop_schema_requires_tool_args_but_not_answer() {
        let required_fields = guided_tool_loop_fields()
            .into_iter()
            .filter_map(|(name, _, _, required)| required.then_some(name))
            .collect::<Vec<_>>();

        assert_eq!(
            required_fields,
            vec!["action", "tool_name", "word", "character"]
        );
    }

    #[test]
    fn initial_final_answer_rejects_empty_answer() {
        let response = parse_guided_tool_loop_response(
            r#"{"action":"final","tool_name":"","word":"","character":""}"#,
            "What time is it?",
            false,
        )
        .expect("answer is optional in the schema");

        let error = initial_final_answer(response).expect_err("empty final answer should fail");

        assert!(error.to_string().contains("without an answer"));
    }

    #[hegel::test]
    fn base64_encoding_uses_expected_length_alphabet_and_padding(tc: TestCase) {
        let data = tc.draw(gs::binary().max_size(128));
        let encoded = base64_encode(&data);

        assert_eq!(encoded.len(), data.len().div_ceil(3) * 4);
        assert!(
            encoded
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '/' || ch == '=')
        );
        match data.len() % 3 {
            0 => assert!(!encoded.ends_with('=')),
            1 => assert!(encoded.ends_with("==")),
            2 => assert!(encoded.ends_with('=')),
            _ => unreachable!(),
        }
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
