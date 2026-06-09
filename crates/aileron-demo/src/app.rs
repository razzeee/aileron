/// aileron-demo — sandboxed GTK4 article summarizer.
use gtk4::prelude::*;
use gtk4::{Box, Button, Entry, Label, Orientation, ScrolledWindow, TextBuffer, TextView};
use libadwaita::prelude::*;
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
    vbox.append(&Label::builder().label("Summary").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&output_view)
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
        summarize_button.connect_clicked(move |_| {
            let (start, end) = source_buffer.bounds();
            let text = source_buffer.text(&start, &end, false).to_string();
            if text.trim().is_empty() {
                return;
            }
            output_buffer.set_text("Summarizing…");

            let output_buffer = output_buffer.clone();
            glib::spawn_future_local(async move {
                let result: Result<String, String> = gio::spawn_blocking(move || {
                    summarize_via_portal(&text).map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|e| Err(format!("thread panic: {e:?}")));

                match result {
                    Ok(summary) => output_buffer.set_text(&summary),
                    Err(e) => output_buffer.set_text(&format!("[error: {e}]")),
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
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn summarize_via_portal(text: &str) -> anyhow::Result<String> {
    use zbus::blocking::Connection;

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.AI",
    )?;

    let session_id: String = proxy.call("CreateSession", &("org.aileron.Demo", "llm.summarize"))?;

    let prompt = format!(
        "Summarize the following article in a few sentences:\n\n{}",
        &text[..text.len().min(4096)]
    );

    let summary: String = proxy.call("Generate", &(&session_id, &prompt))?;
    let _: () = proxy.call("EndSession", &(&session_id,))?;

    Ok(summary)
}
