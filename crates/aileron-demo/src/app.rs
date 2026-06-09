/// aileron-demo — sandboxed GTK4 article summarizer.
///
/// Flatpak-sandboxed app that calls the AI portal to summarize article text.
/// All AI inference is routed through `org.freedesktop.portal.AI` — no direct
/// connection to the daemon socket.

use gtk4::prelude::*;
use libadwaita::prelude::*;
use libadwaita::{Application, ApplicationWindow, HeaderBar, ToolbarView};
use gtk4::{Box, Button, Entry, Label, Orientation, ScrolledWindow, TextView, TextBuffer};

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
    // ── UI layout ─────────────────────────────────────────────────────────────
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

    // Editable text area (source)
    let source_buffer = TextBuffer::new(None);
    let source_view = TextView::builder()
        .buffer(&source_buffer)
        .editable(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    let source_scroll = ScrolledWindow::builder()
        .child(&source_view)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Article text").xalign(0.0).build());
    vbox.append(&source_scroll);

    // Summarize button
    let summarize_button = Button::builder()
        .label("Summarize")
        .css_classes(vec!["suggested-action"])
        .build();
    vbox.append(&summarize_button);

    // Output view
    let output_buffer = TextBuffer::new(None);
    let output_view = TextView::builder()
        .buffer(&output_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    let output_scroll = ScrolledWindow::builder()
        .child(&output_view)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Summary").xalign(0.0).build());
    vbox.append(&output_scroll);

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
            std::thread::spawn(move || {
                match fetch_article_text(&url) {
                    Ok(text) => {
                        glib::MainContext::default().invoke(move || {
                            source_buffer.set_text(&text);
                        });
                    }
                    Err(e) => {
                        let msg = format!("[fetch error: {e}]");
                        glib::MainContext::default().invoke(move || {
                            source_buffer.set_text(&msg);
                        });
                    }
                }
            });
        });
    }

    // ── Summarize handler ─────────────────────────────────────────────────────
    {
        let source_buffer = source_buffer.clone();
        let output_buffer = output_buffer.clone();
        summarize_button.connect_clicked(move |_| {
            let (start, end) = source_buffer.bounds();
            let text = source_buffer.text(&start, &end, false).to_string();
            if text.trim().is_empty() {
                return;
            }

            let output_buffer = output_buffer.clone();
            output_buffer.set_text("Summarizing…");

            std::thread::spawn(move || {
                match summarize_via_portal(&text) {
                    Ok(summary) => {
                        glib::MainContext::default().invoke(move || {
                            output_buffer.set_text(&summary);
                        });
                    }
                    Err(e) => {
                        let msg = format!("[error: {e}]");
                        glib::MainContext::default().invoke(move || {
                            output_buffer.set_text(&msg);
                        });
                    }
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

/// Fetch article text over HTTP and strip HTML tags.
fn fetch_article_text(url: &str) -> anyhow::Result<String> {
    let response = reqwest::blocking::get(url)?;
    let html = response.text()?;
    Ok(strip_html(&html))
}

/// Very lightweight HTML stripping (for demo purposes).
fn strip_html(html: &str) -> String {
    let mut output = String::with_capacity(html.len());
    let mut inside_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => {
                inside_tag = false;
                output.push(' ');
            }
            _ if !inside_tag => output.push(ch),
            _ => {}
        }
    }
    // Collapse whitespace.
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Call the AI portal to summarize text.
///
/// Inside a Flatpak sandbox the app cannot reach the Varlink socket directly;
/// it goes through D-Bus → xdg-desktop-portal → aileron-portal → aileron-daemon.
fn summarize_via_portal(text: &str) -> anyhow::Result<String> {
    use zbus::blocking::Connection;

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.AI",
    )?;

    // Create session.
    let session_id: String = proxy.call("CreateSession", &("org.aileron.Demo", "llm.summarize"))?;

    let prompt = format!(
        "Summarize the following article in a few sentences:\n\n{}",
        &text[..text.len().min(4096)]
    );

    // Generate summary (prompt is plain text; the portal accepts String args).
    let summary: String = proxy.call("Generate", &(&session_id, &prompt))?;

    // End session.
    let _: () = proxy.call("EndSession", &(&session_id,))?;

    Ok(summary)
}
