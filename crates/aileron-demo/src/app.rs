/// aileron-demo — sandboxed GTK4 article summarizer.
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CheckButton, Entry, Label, Orientation, ScrolledWindow, Spinner,
    TextBuffer, TextView,
};
use libadwaita::{Application, ApplicationWindow, HeaderBar, ToolbarView};

pub fn build_app() -> Application {
    let app = Application::builder()
        .application_id("org.aileron.Demo")
        .build();

    app.connect_activate(|app| {
        build_window(app);
    });

    app
}

fn build_window(app: &Application) {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    // URL row
    let url_entry = Entry::builder()
        .placeholder_text("https://example.com/article")
        .hexpand(true)
        .build();
    let fetch_button = Button::with_label("Fetch");
    let url_row = Box::new(Orientation::Horizontal, 8);
    url_row.append(&url_entry);
    url_row.append(&fetch_button);
    vbox.append(&url_row);

    // Source text area
    let source_buffer = TextBuffer::new(None);
    let source_view = TextView::builder()
        .buffer(&source_buffer)
        .editable(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Article text").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&source_view)
            .vexpand(true)
            .build(),
    );

    // Mode switch + action button
    let mode_row = Box::new(Orientation::Horizontal, 8);
    mode_row.append(&Label::builder().label("Mode").xalign(0.0).build());
    let streaming_mode = CheckButton::with_label("Streaming Summary");
    streaming_mode.set_active(true);
    let guided_mode = CheckButton::with_label("Guided JSON");
    guided_mode.set_group(Some(&streaming_mode));
    mode_row.append(&streaming_mode);
    mode_row.append(&guided_mode);
    vbox.append(&mode_row);

    let summarize_button = Button::builder()
        .label("Summarize")
        .css_classes(vec!["suggested-action"])
        .build();
    vbox.append(&summarize_button);

    {
        let summarize_button = summarize_button.clone();
        streaming_mode.connect_toggled(move |button| {
            if button.is_active() {
                summarize_button.set_label("Summarize");
            }
        });
    }

    {
        let summarize_button = summarize_button.clone();
        guided_mode.connect_toggled(move |button| {
            if button.is_active() {
                summarize_button.set_label("Generate Guided JSON");
            }
        });
    }

    // Output view
    let output_buffer = TextBuffer::new(None);
    let status_row = Box::new(Orientation::Horizontal, 12);
    status_row.add_css_class("card");
    status_row.set_margin_bottom(8);
    status_row.set_margin_top(4);
    status_row.set_margin_start(0);
    status_row.set_margin_end(0);
    status_row.set_height_request(72);

    let status_spinner = Spinner::new();
    status_spinner.set_spinning(false);
    status_spinner.set_margin_start(14);
    status_spinner.set_valign(Align::Center);
    status_row.append(&status_spinner);

    let status_text = Box::new(Orientation::Vertical, 2);
    status_text.set_valign(Align::Center);
    status_text.set_margin_top(10);
    status_text.set_margin_bottom(10);
    status_text.set_margin_end(14);
    let status_title = Label::builder()
        .label("Ready")
        .xalign(0.0)
        .css_classes(vec!["heading"])
        .build();
    let status_detail = Label::builder()
        .label("Paste article text, then summarize it locally.")
        .xalign(0.0)
        .wrap(true)
        .build();
    status_text.append(&status_title);
    status_text.append(&status_detail);
    status_row.append(&status_text);

    let output_view = TextView::builder()
        .buffer(&output_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Summary").xalign(0.0).build());
    vbox.append(&status_row);
    vbox.append(
        &ScrolledWindow::builder()
            .child(&output_view)
            .min_content_height(240)
            .vexpand(true)
            .build(),
    );

    // ── Fetch handler ─────────────────────────────────────────────────────────
    {
        let url_entry = url_entry.clone();
        let source_buffer = source_buffer.clone();
        fetch_button.connect_clicked(move |_| {
            let url = url_entry.text().to_string();
            if url.is_empty() {
                return;
            }
            let source_buffer = source_buffer.clone();
            glib::spawn_future_local(async move {
                let result: Result<String, String> = gio::spawn_blocking(move || {
                    fetch_article_text(&url).map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|e| Err(format!("thread panic: {e:?}")));

                match result {
                    Ok(text) => source_buffer.set_text(&text),
                    Err(e) => source_buffer.set_text(&format!("[fetch error: {e}]")),
                }
            });
        });
    }

    // ── Summarize handler ─────────────────────────────────────────────────────
    {
        let source_buffer = source_buffer.clone();
        let output_buffer = output_buffer.clone();
        let summarize_button_for_click = summarize_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        let guided_mode = guided_mode.clone();
        summarize_button.connect_clicked(move |_| {
            let (start, end) = source_buffer.bounds();
            let text = source_buffer.text(&start, &end, false).to_string();
            if text.trim().is_empty() {
                return;
            }
            let mode = if guided_mode.is_active() {
                DemoMode::Guided
            } else {
                DemoMode::Streaming
            };
            output_buffer.set_text("");
            summarize_button_for_click.set_sensitive(false);
            summarize_button_for_click.set_label(mode.busy_label());
            status_spinner.start();
            status_title.set_text(mode.initial_title());
            status_detail.set_text(mode.initial_detail());

            // Channel: background thread sends tokens; glib main loop appends them.
            let (tx, rx) = std::sync::mpsc::channel::<DemoEvent>();

            let output_buffer_clone = output_buffer.clone();
            let summarize_button = summarize_button_for_click.clone();
            let status_spinner = status_spinner.clone();
            let status_title = status_title.clone();
            let status_detail = status_detail.clone();
            let mut saw_token = false;
            // Poll the receiver on the main loop.
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                loop {
                    match rx.try_recv() {
                        Ok(DemoEvent::Phase(phase)) => {
                            status_title.set_text(phase.title());
                            status_detail.set_text(phase.detail());
                            if phase.is_active() {
                                status_spinner.start();
                            } else {
                                status_spinner.stop();
                            }
                        }
                        Ok(DemoEvent::Status(message)) => {
                            status_title.set_text("Loading model");
                            status_detail.set_text(&message);
                        }
                        Ok(DemoEvent::Token(token)) => {
                            if !saw_token {
                                saw_token = true;
                                status_title.set_text("Streaming response");
                                status_detail.set_text("Appending tokens as they arrive.");
                            }
                            output_buffer_clone.insert(&mut output_buffer_clone.end_iter(), &token);
                        }
                        Ok(DemoEvent::Json(content)) => {
                            status_title.set_text("Validated JSON received");
                            status_detail
                                .set_text("Guided generation returned schema-checked JSON.");
                            output_buffer_clone.set_text(&content);
                        }
                        Ok(DemoEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Summary failed");
                            status_detail.set_text(&message);
                            summarize_button.set_sensitive(true);
                            summarize_button.set_label(mode.ready_label());
                            return glib::ControlFlow::Break;
                        }
                        Ok(DemoEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text(mode.complete_title());
                            status_detail.set_text(mode.complete_detail());
                            summarize_button.set_sensitive(true);
                            summarize_button.set_label(mode.ready_label());
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Summary interrupted");
                            status_detail
                                .set_text("The model response channel closed unexpectedly.");
                            summarize_button.set_sensitive(true);
                            summarize_button.set_label(mode.ready_label());
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            // Background thread: call StreamResponse and listen for signals.
            let error_tx = tx.clone();
            std::thread::spawn(move || {
                let result = match mode {
                    DemoMode::Streaming => summarize_streaming(&text, tx),
                    DemoMode::Guided => summarize_guided(&text, tx),
                };
                if let Err(e) = result {
                    eprintln!("[aileron-demo] summarize error: {e}");
                    let _ = error_tx.send(DemoEvent::Error(e.to_string()));
                }
            });
        });
    }

    // ── Window ────────────────────────────────────────────────────────────────
    let header = HeaderBar::new();
    let toolbar_view = ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&vbox));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aileron Demo — Article Summarizer")
        .default_width(700)
        .default_height(700)
        .content(&toolbar_view)
        .build();

    window.present();
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
    Error(String),
    Done,
}

#[derive(Clone, Copy)]
enum DemoMode {
    Streaming,
    Guided,
}

impl DemoMode {
    fn ready_label(&self) -> &'static str {
        match self {
            DemoMode::Streaming => "Summarize",
            DemoMode::Guided => "Generate Guided JSON",
        }
    }

    fn busy_label(&self) -> &'static str {
        match self {
            DemoMode::Streaming => "Summarizing...",
            DemoMode::Guided => "Generating JSON...",
        }
    }

    fn initial_title(&self) -> &'static str {
        match self {
            DemoMode::Streaming => "Creating session",
            DemoMode::Guided => "Creating guided session",
        }
    }

    fn initial_detail(&self) -> &'static str {
        match self {
            DemoMode::Streaming => "Asking the portal to open a language model session...",
            DemoMode::Guided => "Preparing a session for schema-guided output...",
        }
    }

    fn complete_title(&self) -> &'static str {
        match self {
            DemoMode::Streaming => "Summary complete",
            DemoMode::Guided => "Guided JSON complete",
        }
    }

    fn complete_detail(&self) -> &'static str {
        match self {
            DemoMode::Streaming => "The local model finished streaming its response.",
            DemoMode::Guided => {
                "The daemon validated the model output against the generated schema."
            }
        }
    }
}

enum DemoPhase {
    CreatingSession,
    WaitingForModel,
    RequestingStream,
    RequestingGuided,
}

impl DemoPhase {
    fn title(&self) -> &'static str {
        match self {
            DemoPhase::CreatingSession => "Creating session",
            DemoPhase::WaitingForModel => "Loading model",
            DemoPhase::RequestingStream => "Starting response",
            DemoPhase::RequestingGuided => "Requesting guided JSON",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            DemoPhase::CreatingSession => "Asking the portal to open a language model session...",
            DemoPhase::WaitingForModel => "Starting the local container if the model is cold...",
            DemoPhase::RequestingStream => "Sending the prompt and waiting for the first token...",
            DemoPhase::RequestingGuided => "Sending field guides and waiting for validated JSON...",
        }
    }

    fn is_active(&self) -> bool {
        true
    }
}

/// Call `StreamResponse` on the portal and forward tokens via `tx`.
/// Sends `Some(token)` for each token, then `None` when done.
fn summarize_streaming(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

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
        "CreateLanguageModelSession",
        &(
            "org.aileron.Demo",
            "llm.summarize",
            "You summarize user-provided text clearly and concisely.",
        ),
    )?;

    drop(loading_thread);
    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;

    let prompt = format!(
        "Summarize the following article in 3-5 sentences:\n\n{}",
        &text[..text.len().min(8192)]
    );

    // Subscribe to TokenReceived on the signal connection.
    let mut token_iter = sig_proxy.receive_signal("TokenReceived")?;

    // StreamResponse returns immediately; tokens arrive as D-Bus signals.
    let options = (512_i64, 0.7_f64, "default");
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

fn summarize_guided(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(DemoEvent::Phase(DemoPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateLanguageModelSession",
        &(
            "org.aileron.Demo",
            "llm.summarize",
            "You extract concise, factual summary data as valid JSON.",
        ),
    )?;

    let prompt = format!(
        "Summarize this article as structured data. Keep the summary short, include 3-5 key points, and set confidence from 0 to 100:\n\n{}",
        &text[..text.len().min(8192)]
    );
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
    let options = (512_i64, 0.2_f64, "default");

    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;
    tx.send(DemoEvent::Phase(DemoPhase::RequestingGuided))?;
    let content: String = proxy.call("RespondGuided", &(&session_id, &prompt, fields, options))?;
    let pretty = serde_json::from_str::<serde_json::Value>(&content)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or(content);
    tx.send(DemoEvent::Json(pretty))?;

    let _: () = proxy.call("EndSession", &(&session_id,))?;
    tx.send(DemoEvent::Done)?;
    Ok(())
}
