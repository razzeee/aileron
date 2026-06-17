/// aileron-demo — sandboxed GTK4 article summarizer.
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CssProvider, DropDown, Entry, FileDialog, Label, Orientation,
    ScrolledWindow, Spinner, TextBuffer, TextView,
};
use libadwaita::{
    Application, ApplicationWindow, HeaderBar, OverlaySplitView, ToolbarView, ViewStack,
    ViewSwitcherSidebar,
};
use serde::Deserialize;
use std::cell::RefCell;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};
use zbus::zvariant::Type;

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
    let text_box = Box::new(Orientation::Vertical, 12);
    text_box.set_margin_top(12);
    text_box.set_margin_bottom(12);
    text_box.set_margin_start(12);
    text_box.set_margin_end(12);

    // URL row
    let url_entry = Entry::builder()
        .placeholder_text("https://example.com/article")
        .hexpand(true)
        .build();
    let fetch_button = Button::with_label("Fetch");
    let url_row = Box::new(Orientation::Horizontal, 8);
    url_row.append(&url_entry);
    url_row.append(&fetch_button);
    text_box.append(&url_row);

    // Source text area
    let source_buffer = TextBuffer::new(None);
    let source_view = TextView::builder()
        .buffer(&source_buffer)
        .editable(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    text_box.append(&Label::builder().label("Article text").xalign(0.0).build());
    text_box.append(
        &ScrolledWindow::builder()
            .child(&source_view)
            .vexpand(true)
            .build(),
    );

    // Mode switch + action button
    let mode_row = Box::new(Orientation::Horizontal, 8);
    mode_row.append(&Label::builder().label("Mode").xalign(0.0).build());
    let mode_dropdown = DropDown::from_strings(&DemoMode::labels());
    mode_dropdown.set_selected(DemoMode::Summarize.index());
    mode_dropdown.set_hexpand(true);
    mode_row.append(&mode_dropdown);
    text_box.append(&mode_row);

    let summarize_button = Button::builder()
        .label("Summarize")
        .css_classes(vec!["suggested-action"])
        .build();
    text_box.append(&summarize_button);

    {
        let summarize_button = summarize_button.clone();
        mode_dropdown.connect_selected_notify(move |dropdown| {
            let mode = DemoMode::from_index(dropdown.selected()).unwrap_or(DemoMode::Summarize);
            summarize_button.set_label(mode.ready_label());
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
        .label("Paste text, then run a local LLM task.")
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
    text_box.append(&Label::builder().label("Output").xalign(0.0).build());
    text_box.append(&status_row);
    text_box.append(
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
        let mode_dropdown = mode_dropdown.clone();
        summarize_button.connect_clicked(move |_| {
            let (start, end) = source_buffer.bounds();
            let text = source_buffer.text(&start, &end, false).to_string();
            if text.trim().is_empty() {
                return;
            }
            let mode =
                DemoMode::from_index(mode_dropdown.selected()).unwrap_or(DemoMode::Summarize);
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
                        Ok(DemoEvent::Text(content)) => {
                            saw_token = true;
                            status_title.set_text("Response received");
                            status_detail.set_text("The local model returned a complete response.");
                            output_buffer_clone.set_text(&content);
                        }
                        Ok(DemoEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Task failed");
                            status_detail.set_text(&message);
                            summarize_button.set_sensitive(true);
                            summarize_button.set_label(mode.ready_label());
                            return glib::ControlFlow::Break;
                        }
                        Ok(DemoEvent::Done) => {
                            status_spinner.stop();
                            if !saw_token && matches!(mode, DemoMode::Summarize) {
                                status_title.set_text("Task failed");
                                status_detail.set_text(
                                    "The local model completed without returning any text.",
                                );
                                summarize_button.set_sensitive(true);
                                summarize_button.set_label(mode.ready_label());
                                return glib::ControlFlow::Break;
                            }
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
                    DemoMode::Summarize => summarize_streaming(&text, tx),
                    DemoMode::Extract => extract_guided(&text, tx),
                    DemoMode::Classify => classify_guided(&text, tx),
                    DemoMode::Translate | DemoMode::Rephrase | DemoMode::Analyze => {
                        respond_text_task(mode, &text, tx)
                    }
                };
                if let Err(e) = result {
                    eprintln!("[aileron-demo] text task error: {e}");
                    let _ = error_tx.send(DemoEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    // ── Window ────────────────────────────────────────────────────────────────
    let stack = ViewStack::new();
    let (chat_page, chat_entry) = build_chat_page();
    let text_page = stack.add_titled(&text_box, Some("text"), "Text Tasks");
    text_page.set_icon_name(Some("text-x-generic-symbolic"));
    let chat_page_meta = stack.add_titled(&chat_page, Some("chat"), "Chat");
    chat_page_meta.set_icon_name(Some("user-available-symbolic"));
    let speech_page = stack.add_titled(&build_speech_page(), Some("speech"), "Speech");
    speech_page.set_icon_name(Some("audio-input-microphone-symbolic"));
    let vision_page = stack.add_titled(&build_vision_page(), Some("vision"), "Vision");
    vision_page.set_icon_name(Some("image-x-generic-symbolic"));
    let embed_page = stack.add_titled(&build_embed_page(), Some("embed"), "Embeddings");
    embed_page.set_icon_name(Some("emblem-documents-symbolic"));
    stack.set_visible_child_name("chat");

    let sidebar = ViewSwitcherSidebar::builder().stack(&stack).build();

    let split_view = OverlaySplitView::new();
    split_view.set_min_sidebar_width(150.0);
    split_view.set_max_sidebar_width(180.0);
    split_view.set_show_sidebar(true);

    let sidebar_header = HeaderBar::new();
    let hide_sidebar_button = Button::builder()
        .icon_name("sidebar-show-symbolic")
        .tooltip_text("Toggle Sidebar")
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
        .tooltip_text("Toggle Sidebar")
        .build();
    {
        let split_view = split_view.clone();
        show_sidebar_button.connect_clicked(move |_| {
            split_view.set_show_sidebar(true);
        });
    }
    content_header.pack_start(&show_sidebar_button);
    content_header.set_title_widget(Some(&Label::new(Some("Aileron Demo"))));
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

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aileron Demo")
        .default_width(860)
        .default_height(700)
        .content(&split_view)
        .build();

    window.present();
    chat_entry.grab_focus();
}

#[derive(Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

fn build_chat_page() -> (gtk4::Widget, Entry) {
    install_chat_css();

    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Send a multi-turn local chat through the stateless llm.chat API.")
            .xalign(0.0)
            .wrap(true)
            .build(),
    );

    let status_row = Box::new(Orientation::Horizontal, 12);
    status_row.add_css_class("card");
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
        .label("Ask a question. The app sends the full message list each turn.")
        .xalign(0.0)
        .wrap(true)
        .build();
    status_text.append(&status_title);
    status_text.append(&status_detail);
    status_row.append(&status_text);
    vbox.append(&status_row);

    let chat_box = Box::new(Orientation::Vertical, 10);
    chat_box.set_margin_top(12);
    chat_box.set_margin_bottom(12);
    chat_box.set_margin_start(24);
    chat_box.set_margin_end(24);
    chat_box.set_valign(Align::Start);
    chat_box.set_vexpand(true);

    render_chat(&chat_box, &[], None);

    let chat_scroller = ScrolledWindow::builder()
        .child(&chat_box)
        .min_content_height(360)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&chat_scroller);

    let input_row = Box::new(Orientation::Horizontal, 8);
    let input_entry = Entry::builder()
        .placeholder_text("Ask the local model...")
        .hexpand(true)
        .build();
    let send_button = Button::builder()
        .label("Send")
        .css_classes(vec!["suggested-action"])
        .build();
    let clear_button = Button::with_label("Clear Chat");
    input_row.append(&input_entry);
    input_row.append(&send_button);
    input_row.append(&clear_button);
    vbox.append(&input_row);

    let history = Rc::new(RefCell::new(Vec::<ChatMessage>::new()));
    let session_id = Rc::new(RefCell::new(None::<String>));

    {
        let history = history.clone();
        let session_id = session_id.clone();
        let input_entry = input_entry.clone();
        let send_button_for_click = send_button.clone();
        let send_button = send_button.clone();
        let clear_button = clear_button.clone();
        let chat_box = chat_box.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        send_button_for_click.connect_clicked(move |_| {
            let text = input_entry.text().trim().to_string();
            if text.is_empty() {
                return;
            }

            input_entry.set_text("");
            send_button.set_sensitive(false);
            clear_button.set_sensitive(false);
            status_spinner.start();
            status_title.set_text("Starting chat turn");
            status_detail.set_text("Sending message history through StreamChat...");

            history.borrow_mut().push(ChatMessage {
                role: "user".to_string(),
                content: text,
            });
            render_chat(&chat_box, &history.borrow(), None);

            let messages = history.borrow().clone();
            let existing_session = session_id.borrow().clone();
            let (tx, rx) = std::sync::mpsc::channel::<ChatEvent>();

            let history_for_rx = history.clone();
            let session_for_rx = session_id.clone();
            let chat_box_for_rx = chat_box.clone();
            let send_button_for_rx = send_button.clone();
            let clear_button_for_rx = clear_button.clone();
            let status_spinner_for_rx = status_spinner.clone();
            let status_title_for_rx = status_title.clone();
            let status_detail_for_rx = status_detail.clone();
            let mut assistant_text = String::new();
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                loop {
                    match rx.try_recv() {
                        Ok(ChatEvent::SessionCreated(id)) => {
                            *session_for_rx.borrow_mut() = Some(id);
                            status_title_for_rx.set_text("Chat session ready");
                            status_detail_for_rx.set_text("The llm.chat session is active.");
                        }
                        Ok(ChatEvent::Token(token)) => {
                            assistant_text.push_str(&token);
                            status_title_for_rx.set_text("Streaming response");
                            status_detail_for_rx.set_text("Appending chat tokens as they arrive.");
                            render_chat(
                                &chat_box_for_rx,
                                &history_for_rx.borrow(),
                                Some(&assistant_text),
                            );
                        }
                        Ok(ChatEvent::Error(message)) => {
                            status_spinner_for_rx.stop();
                            status_title_for_rx.set_text("Chat failed");
                            status_detail_for_rx.set_text(&message);
                            send_button_for_rx.set_sensitive(true);
                            clear_button_for_rx.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(ChatEvent::Done) => {
                            if !assistant_text.trim().is_empty() {
                                history_for_rx.borrow_mut().push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: assistant_text.clone(),
                                });
                            }
                            render_chat(&chat_box_for_rx, &history_for_rx.borrow(), None);
                            status_spinner_for_rx.stop();
                            status_title_for_rx.set_text("Response complete");
                            status_detail_for_rx
                                .set_text("The app kept history locally and sent it explicitly.");
                            send_button_for_rx.set_sensitive(true);
                            clear_button_for_rx.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner_for_rx.stop();
                            status_title_for_rx.set_text("Chat interrupted");
                            status_detail_for_rx
                                .set_text("The chat response channel closed unexpectedly.");
                            send_button_for_rx.set_sensitive(true);
                            clear_button_for_rx.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = chat_stream(existing_session, messages, tx) {
                    eprintln!("[aileron-demo] chat error: {e}");
                    let _ = error_tx.send(ChatEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    {
        let send_button = send_button.clone();
        input_entry.connect_activate(move |_| {
            if send_button.is_sensitive() {
                send_button.emit_clicked();
            }
        });
    }

    {
        let history = history.clone();
        let session_id = session_id.clone();
        let chat_box = chat_box.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        clear_button.connect_clicked(move |_| {
            if let Some(id) = session_id.borrow_mut().take() {
                std::thread::spawn(move || {
                    let _ = end_chat_session(&id);
                });
            }
            history.borrow_mut().clear();
            render_chat(&chat_box, &history.borrow(), None);
            status_spinner.stop();
            status_title.set_text("Ready");
            status_detail
                .set_text("Chat cleared. The next message creates a fresh llm.chat session.");
        });
    }

    (vbox.upcast(), input_entry)
}

fn install_chat_css() {
    let Some(display) = gtk4::gdk::Display::default() else {
        return;
    };

    let provider = CssProvider::new();
    provider.load_from_string(
        r#"
        .chat-empty-state {
            color: alpha(currentColor, 0.65);
        }

        .chat-bubble {
            border-radius: 18px;
            padding: 10px 13px;
        }

        .chat-bubble-user {
            background: @accent_bg_color;
            color: @accent_fg_color;
            border-bottom-right-radius: 4px;
        }

        .chat-bubble-assistant {
            background: @card_bg_color;
            color: @window_fg_color;
            border: 1px solid @borders;
            border-bottom-left-radius: 4px;
        }
        "#,
    );
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn render_chat(chat_box: &Box, history: &[ChatMessage], pending_assistant: Option<&str>) {
    while let Some(child) = chat_box.first_child() {
        chat_box.remove(&child);
    }

    if history.is_empty() && pending_assistant.is_none() {
        let empty_state = Label::builder()
            .label("No messages yet.")
            .halign(Align::Center)
            .valign(Align::Center)
            .vexpand(true)
            .css_classes(vec!["chat-empty-state"])
            .build();
        chat_box.append(&empty_state);
        return;
    }

    for message in history {
        append_chat_bubble(chat_box, &message.role, &message.content);
    }

    if let Some(content) = pending_assistant {
        append_chat_bubble(chat_box, "assistant", content);
    }
}

fn append_chat_bubble(chat_box: &Box, role: &str, content: &str) {
    let is_assistant = role == "assistant";
    let row = Box::new(Orientation::Horizontal, 0);
    row.set_hexpand(true);
    row.set_halign(if is_assistant {
        Align::Start
    } else {
        Align::End
    });
    row.set_margin_top(2);
    row.set_margin_bottom(2);

    let bubble = Box::new(Orientation::Vertical, 0);
    bubble.add_css_class("chat-bubble");
    bubble.add_css_class(if is_assistant {
        "chat-bubble-assistant"
    } else {
        "chat-bubble-user"
    });

    let label = Label::builder()
        .label(content)
        .wrap(true)
        .wrap_mode(gtk4::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .max_width_chars(72)
        .build();
    bubble.append(&label);
    row.append(&bubble);
    chat_box.append(&row);
}

struct Recording {
    child: Child,
    path: PathBuf,
}

fn build_speech_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Record microphone audio, then transcribe it or translate it to English through the ASR portal path.")
            .xalign(0.0)
            .wrap(true)
            .build(),
    );

    let button_row = Box::new(Orientation::Horizontal, 8);
    let record_button = Button::builder()
        .label("Record")
        .css_classes(vec!["suggested-action"])
        .build();
    let stop_button = Button::with_label("Stop");
    stop_button.set_sensitive(false);
    let transcribe_button = Button::with_label("Transcribe Audio");
    transcribe_button.set_sensitive(false);
    let translate_button = Button::with_label("Translate Audio");
    translate_button.set_sensitive(false);
    button_row.append(&record_button);
    button_row.append(&stop_button);
    button_row.append(&transcribe_button);
    button_row.append(&translate_button);
    vbox.append(&button_row);

    let status_row = Box::new(Orientation::Horizontal, 12);
    status_row.add_css_class("card");
    status_row.set_margin_bottom(8);
    status_row.set_margin_top(4);
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
        .label("Use Record to capture 16 kHz mono f32 audio with pw-record.")
        .xalign(0.0)
        .wrap(true)
        .build();
    status_text.append(&status_title);
    status_text.append(&status_detail);
    status_row.append(&status_text);
    vbox.append(&status_row);

    let transcript_buffer = TextBuffer::new(None);
    let transcript_view = TextView::builder()
        .buffer(&transcript_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Transcript").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&transcript_view)
            .min_content_height(320)
            .vexpand(true)
            .build(),
    );

    let recording = Rc::new(RefCell::new(None::<Recording>));
    let last_audio = Rc::new(RefCell::new(None::<PathBuf>));

    {
        let recording = recording.clone();
        let last_audio = last_audio.clone();
        let record_button_for_click = record_button.clone();
        let record_button = record_button.clone();
        let stop_button = stop_button.clone();
        let transcribe_button = transcribe_button.clone();
        let translate_button = translate_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        record_button_for_click.connect_clicked(move |_| {
            if recording.borrow().is_some() {
                return;
            }

            let path = temp_audio_path();
            let child = match Command::new("pw-record")
                .args([
                    "--raw",
                    "--rate",
                    "16000",
                    "--channels",
                    "1",
                    "--format",
                    "f32",
                ])
                .arg(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    status_spinner.stop();
                    status_title.set_text("Recording unavailable");
                    status_detail.set_text(&format!("Could not start pw-record: {e}"));
                    return;
                }
            };

            *recording.borrow_mut() = Some(Recording {
                child,
                path: path.clone(),
            });
            *last_audio.borrow_mut() = None;
            status_spinner.start();
            status_title.set_text("Recording microphone");
            status_detail.set_text("Speak now. Stop when you are ready to transcribe.");
            record_button.set_sensitive(false);
            stop_button.set_sensitive(true);
            transcribe_button.set_sensitive(false);
            translate_button.set_sensitive(false);
        });
    }

    {
        let recording = recording.clone();
        let last_audio = last_audio.clone();
        let record_button = record_button.clone();
        let stop_button_for_click = stop_button.clone();
        let stop_button = stop_button.clone();
        let transcribe_button = transcribe_button.clone();
        let translate_button = translate_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        stop_button_for_click.connect_clicked(move |_| {
            let Some(mut current) = recording.borrow_mut().take() else {
                return;
            };

            let _ = current.child.kill();
            let _ = current.child.wait();
            *last_audio.borrow_mut() = Some(current.path);
            status_spinner.stop();
            status_title.set_text("Recording saved");
            status_detail
                .set_text("Audio is ready. Transcribe or translate it through the portal.");
            record_button.set_sensitive(true);
            stop_button.set_sensitive(false);
            transcribe_button.set_sensitive(true);
            translate_button.set_sensitive(true);
        });
    }

    let wire_asr_action = {
        let last_audio = last_audio.clone();
        let transcript_buffer = transcript_buffer.clone();
        let transcribe_button = transcribe_button.clone();
        let translate_button = translate_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        move |action_button: &Button, use_case: &'static str, verb: &'static str| {
            let last_audio = last_audio.clone();
            let transcript_buffer = transcript_buffer.clone();
            let transcribe_button = transcribe_button.clone();
            let translate_button = translate_button.clone();
            let status_spinner = status_spinner.clone();
            let status_title = status_title.clone();
            let status_detail = status_detail.clone();
            action_button.connect_clicked(move |_| {
                let Some(path) = last_audio.borrow().clone() else {
                    status_title.set_text("No recording");
                    status_detail.set_text(&format!("Record audio before {verb}."));
                    return;
                };

                transcript_buffer.set_text("");
                transcribe_button.set_sensitive(false);
                translate_button.set_sensitive(false);
                status_spinner.start();
                status_title.set_text("Creating ASR session");
                status_detail.set_text(&format!(
                    "Opening an {use_case} session through the portal..."
                ));

                let (tx, rx) = std::sync::mpsc::channel::<SpeechEvent>();
                let transcript_buffer = transcript_buffer.clone();
                let transcribe_button = transcribe_button.clone();
                let translate_button = translate_button.clone();
                let status_spinner = status_spinner.clone();
                let status_title = status_title.clone();
                let status_detail = status_detail.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                    loop {
                        match rx.try_recv() {
                            Ok(SpeechEvent::Phase(phase)) => {
                                status_title.set_text(phase.title());
                                status_detail.set_text(phase.detail());
                                status_spinner.start();
                            }
                            Ok(SpeechEvent::Transcript(text)) => {
                                transcript_buffer.set_text(&text);
                            }
                            Ok(SpeechEvent::Error(message)) => {
                                status_spinner.stop();
                                status_title.set_text("ASR request failed");
                                status_detail.set_text(&message);
                                transcribe_button.set_sensitive(true);
                                translate_button.set_sensitive(true);
                                return glib::ControlFlow::Break;
                            }
                            Ok(SpeechEvent::Done) => {
                                status_spinner.stop();
                                status_title.set_text("ASR result complete");
                                status_detail.set_text("ASR returned text through the portal.");
                                transcribe_button.set_sensitive(true);
                                translate_button.set_sensitive(true);
                                return glib::ControlFlow::Break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                status_spinner.stop();
                                status_title.set_text("ASR request interrupted");
                                status_detail
                                    .set_text("The ASR response channel closed unexpectedly.");
                                transcribe_button.set_sensitive(true);
                                translate_button.set_sensitive(true);
                                return glib::ControlFlow::Break;
                            }
                        }
                    }
                    glib::ControlFlow::Continue
                });

                let error_tx = tx.clone();
                std::thread::spawn(move || {
                    if let Err(e) = transcribe_recording(&path, use_case, tx) {
                        eprintln!("[aileron-demo] asr error: {e}");
                        let _ = error_tx.send(SpeechEvent::Error(friendly_error(&e)));
                    }
                });
            });
        }
    };

    wire_asr_action(&transcribe_button, "asr.transcribe", "transcribing");
    wire_asr_action(&translate_button, "asr.translate", "translating");

    vbox.upcast()
}

fn build_vision_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Describe, extract text from, or segment an image through the vision portal path. Select a PNG/JPEG file or paste base64 image bytes.")
            .xalign(0.0)
            .wrap(true)
            .build(),
    );

    let selected_image = Rc::new(RefCell::new(None::<Vec<u8>>));

    let button_row = Box::new(Orientation::Horizontal, 8);
    let choose_button = Button::with_label("Choose Image");
    let describe_button = Button::builder()
        .label("Describe Image")
        .css_classes(vec!["suggested-action"])
        .build();
    let segment_button = Button::with_label("Segment Objects");
    let ocr_button = Button::with_label("Extract Text");
    button_row.append(&choose_button);
    button_row.append(&describe_button);
    button_row.append(&ocr_button);
    button_row.append(&segment_button);
    vbox.append(&button_row);

    let selected_label = Label::builder()
        .label("No file selected. Paste base64 below or choose an image.")
        .xalign(0.0)
        .wrap(true)
        .build();
    vbox.append(&selected_label);

    let paste_buffer = TextBuffer::new(None);
    let paste_view = TextView::builder()
        .buffer(&paste_buffer)
        .editable(true)
        .wrap_mode(gtk4::WrapMode::Char)
        .hexpand(true)
        .vexpand(false)
        .build();
    vbox.append(
        &Label::builder()
            .label("Pasted base64 image")
            .xalign(0.0)
            .build(),
    );
    vbox.append(
        &ScrolledWindow::builder()
            .child(&paste_view)
            .min_content_height(120)
            .build(),
    );

    let status_row = Box::new(Orientation::Horizontal, 12);
    status_row.add_css_class("card");
    status_row.set_margin_bottom(8);
    status_row.set_margin_top(4);
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
        .label("Choose or paste an image, then describe it locally.")
        .xalign(0.0)
        .wrap(true)
        .build();
    status_text.append(&status_title);
    status_text.append(&status_detail);
    status_row.append(&status_text);
    vbox.append(&status_row);

    let description_buffer = TextBuffer::new(None);
    let description_view = TextView::builder()
        .buffer(&description_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Description").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&description_view)
            .min_content_height(160)
            .vexpand(true)
            .build(),
    );

    let ocr_buffer = TextBuffer::new(None);
    let ocr_view = TextView::builder()
        .buffer(&ocr_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Extracted text").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&ocr_view)
            .min_content_height(140)
            .vexpand(true)
            .build(),
    );

    let segments_buffer = TextBuffer::new(None);
    let segments_view = TextView::builder()
        .buffer(&segments_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Segments").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&segments_view)
            .min_content_height(140)
            .vexpand(true)
            .build(),
    );

    {
        let selected_image = selected_image.clone();
        let selected_label = selected_label.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        choose_button.connect_clicked(move |_| {
            let dialog = FileDialog::builder().title("Choose image").build();
            let selected_image = selected_image.clone();
            let selected_label = selected_label.clone();
            let status_title = status_title.clone();
            let status_detail = status_detail.clone();
            dialog.open(
                None::<&gtk4::Window>,
                None::<&gio::Cancellable>,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        status_title.set_text("Could not read image");
                        status_detail.set_text("Selected file has no local filesystem path.");
                        return;
                    };
                    match std::fs::read(&path) {
                        Ok(bytes) => {
                            *selected_image.borrow_mut() = Some(bytes);
                            selected_label.set_text(&format!("Selected: {}", path.display()));
                            status_title.set_text("Image selected");
                            status_detail.set_text(
                                "Use Describe Image or Segment Objects to send it through the vision portal.",
                            );
                        }
                        Err(e) => {
                            status_title.set_text("Could not read image");
                            status_detail.set_text(&e.to_string());
                        }
                    }
                },
            );
        });
    }

    {
        let selected_image = selected_image.clone();
        let paste_buffer = paste_buffer.clone();
        let description_buffer = description_buffer.clone();
        let describe_button_for_click = describe_button.clone();
        let ocr_button_for_click = ocr_button.clone();
        let segment_button_for_click = segment_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        describe_button.connect_clicked(move |_| {
            let image_b64 = if let Some(bytes) = selected_image.borrow().clone() {
                base64_encode(&bytes)
            } else {
                let (start, end) = paste_buffer.bounds();
                paste_buffer
                    .text(&start, &end, false)
                    .trim()
                    .replace(['\n', '\r', ' ', '\t'], "")
            };

            if image_b64.is_empty() {
                status_title.set_text("No image input");
                status_detail.set_text("Choose an image file or paste base64 image bytes first.");
                return;
            }

            description_buffer.set_text("");
            describe_button_for_click.set_sensitive(false);
            ocr_button_for_click.set_sensitive(false);
            segment_button_for_click.set_sensitive(false);
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.describe session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let description_buffer = description_buffer.clone();
            let describe_button = describe_button_for_click.clone();
            let ocr_button = ocr_button_for_click.clone();
            let segment_button = segment_button_for_click.clone();
            let status_spinner = status_spinner.clone();
            let status_title = status_title.clone();
            let status_detail = status_detail.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                loop {
                    match rx.try_recv() {
                        Ok(VisionEvent::Phase(phase)) => {
                            status_title.set_text(phase.title());
                            status_detail.set_text(phase.detail());
                            status_spinner.start();
                        }
                        Ok(VisionEvent::Description(text)) => {
                            description_buffer.set_text(&text);
                        }
                        Ok(VisionEvent::Ocr(_)) => {}
                        Ok(VisionEvent::Segments(_)) => {}
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Description failed");
                            status_detail.set_text(&message);
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Description complete");
                            status_detail
                                .set_text("Vision returned a description through the portal.");
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Description interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = describe_image(&image_b64, tx) {
                    eprintln!("[aileron-demo] describe error: {e}");
                    let _ = error_tx.send(VisionEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    {
        let selected_image = selected_image.clone();
        let paste_buffer = paste_buffer.clone();
        let ocr_buffer = ocr_buffer.clone();
        let describe_button_for_click = describe_button.clone();
        let ocr_button_for_click = ocr_button.clone();
        let segment_button_for_click = segment_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        ocr_button.connect_clicked(move |_| {
            let image_b64 = if let Some(bytes) = selected_image.borrow().clone() {
                base64_encode(&bytes)
            } else {
                let (start, end) = paste_buffer.bounds();
                paste_buffer
                    .text(&start, &end, false)
                    .trim()
                    .replace(['\n', '\r', ' ', '\t'], "")
            };

            if image_b64.is_empty() {
                status_title.set_text("No image input");
                status_detail.set_text("Choose an image file or paste base64 image bytes first.");
                return;
            }

            ocr_buffer.set_text("");
            describe_button_for_click.set_sensitive(false);
            ocr_button_for_click.set_sensitive(false);
            segment_button_for_click.set_sensitive(false);
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.ocr session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let ocr_buffer = ocr_buffer.clone();
            let describe_button = describe_button_for_click.clone();
            let ocr_button = ocr_button_for_click.clone();
            let segment_button = segment_button_for_click.clone();
            let status_spinner = status_spinner.clone();
            let status_title = status_title.clone();
            let status_detail = status_detail.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                loop {
                    match rx.try_recv() {
                        Ok(VisionEvent::Phase(phase)) => {
                            status_title.set_text(phase.title());
                            status_detail.set_text(phase.detail());
                            status_spinner.start();
                        }
                        Ok(VisionEvent::Description(_)) => {}
                        Ok(VisionEvent::Ocr(text)) => {
                            ocr_buffer.set_text(&text);
                        }
                        Ok(VisionEvent::Segments(_)) => {}
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Text extraction failed");
                            status_detail.set_text(&message);
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Text extraction complete");
                            status_detail
                                .set_text("Vision returned extracted text through the portal.");
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Text extraction interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = ocr_image(&image_b64, tx) {
                    eprintln!("[aileron-demo] ocr error: {e}");
                    let _ = error_tx.send(VisionEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    {
        let selected_image = selected_image.clone();
        let paste_buffer = paste_buffer.clone();
        let segments_buffer = segments_buffer.clone();
        let describe_button_for_click = describe_button.clone();
        let ocr_button_for_click = ocr_button.clone();
        let segment_button_for_click = segment_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        segment_button.connect_clicked(move |_| {
            let image_b64 = if let Some(bytes) = selected_image.borrow().clone() {
                base64_encode(&bytes)
            } else {
                let (start, end) = paste_buffer.bounds();
                paste_buffer
                    .text(&start, &end, false)
                    .trim()
                    .replace(['\n', '\r', ' ', '\t'], "")
            };

            if image_b64.is_empty() {
                status_title.set_text("No image input");
                status_detail.set_text("Choose an image file or paste base64 image bytes first.");
                return;
            }

            segments_buffer.set_text("");
            describe_button_for_click.set_sensitive(false);
            ocr_button_for_click.set_sensitive(false);
            segment_button_for_click.set_sensitive(false);
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.segment session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let segments_buffer = segments_buffer.clone();
            let describe_button = describe_button_for_click.clone();
            let ocr_button = ocr_button_for_click.clone();
            let segment_button = segment_button_for_click.clone();
            let status_spinner = status_spinner.clone();
            let status_title = status_title.clone();
            let status_detail = status_detail.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                loop {
                    match rx.try_recv() {
                        Ok(VisionEvent::Phase(phase)) => {
                            status_title.set_text(phase.title());
                            status_detail.set_text(phase.detail());
                            status_spinner.start();
                        }
                        Ok(VisionEvent::Description(_)) => {}
                        Ok(VisionEvent::Ocr(_)) => {}
                        Ok(VisionEvent::Segments(segments)) => {
                            segments_buffer.set_text(&format_segments(&segments));
                        }
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Segmentation failed");
                            status_detail.set_text(&message);
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Segmentation complete");
                            status_detail.set_text("Vision returned normalized object boxes.");
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Segmentation interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            describe_button.set_sensitive(true);
                            ocr_button.set_sensitive(true);
                            segment_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = segment_image(&image_b64, tx) {
                    eprintln!("[aileron-demo] segment error: {e}");
                    let _ = error_tx.send(VisionEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    vbox.upcast()
}

fn build_embed_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Turn text into an embedding vector through the llm.embed portal path. Useful for semantic search, clustering, and retrieval.")
            .xalign(0.0)
            .wrap(true)
            .build(),
    );

    let input_buffer = TextBuffer::new(None);
    input_buffer.set_text("The quick brown fox jumps over the lazy dog.");
    let input_view = TextView::builder()
        .buffer(&input_buffer)
        .editable(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(false)
        .build();
    vbox.append(&Label::builder().label("Text to embed").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&input_view)
            .min_content_height(120)
            .build(),
    );

    let button_row = Box::new(Orientation::Horizontal, 8);
    let embed_button = Button::builder()
        .label("Embed Text")
        .css_classes(vec!["suggested-action"])
        .build();
    button_row.append(&embed_button);
    vbox.append(&button_row);

    let status_row = Box::new(Orientation::Horizontal, 12);
    status_row.add_css_class("card");
    status_row.set_margin_bottom(8);
    status_row.set_margin_top(4);
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
        .label("Enter text, then embed it locally.")
        .xalign(0.0)
        .wrap(true)
        .build();
    status_text.append(&status_title);
    status_text.append(&status_detail);
    status_row.append(&status_text);
    vbox.append(&status_row);

    let output_buffer = TextBuffer::new(None);
    let output_view = TextView::builder()
        .buffer(&output_buffer)
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(
        &Label::builder()
            .label("Embedding vector")
            .xalign(0.0)
            .build(),
    );
    vbox.append(
        &ScrolledWindow::builder()
            .child(&output_view)
            .min_content_height(200)
            .vexpand(true)
            .build(),
    );

    {
        let input_buffer = input_buffer.clone();
        let output_buffer = output_buffer.clone();
        let embed_button_for_click = embed_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        embed_button.connect_clicked(move |_| {
            let (start, end) = input_buffer.bounds();
            let text = input_buffer.text(&start, &end, false).trim().to_string();
            if text.is_empty() {
                status_title.set_text("No text input");
                status_detail.set_text("Enter some text to embed first.");
                return;
            }

            output_buffer.set_text("");
            embed_button_for_click.set_sensitive(false);
            status_spinner.start();
            status_title.set_text("Creating embedding session");
            status_detail.set_text("Opening an llm.embed session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<EmbedEvent>();
            let output_buffer = output_buffer.clone();
            let embed_button = embed_button_for_click.clone();
            let status_spinner = status_spinner.clone();
            let status_title = status_title.clone();
            let status_detail = status_detail.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                loop {
                    match rx.try_recv() {
                        Ok(EmbedEvent::Phase(phase)) => {
                            status_title.set_text(phase.title());
                            status_detail.set_text(phase.detail());
                            status_spinner.start();
                        }
                        Ok(EmbedEvent::Embedding(vector)) => {
                            output_buffer.set_text(&format_embedding(&vector));
                        }
                        Ok(EmbedEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Embedding failed");
                            status_detail.set_text(&message);
                            embed_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(EmbedEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Embedding complete");
                            status_detail.set_text("The local model returned an embedding vector.");
                            embed_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Embedding interrupted");
                            status_detail
                                .set_text("The embedding response channel closed unexpectedly.");
                            embed_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = embed_text(&text, tx) {
                    eprintln!("[aileron-demo] embed error: {e}");
                    let _ = error_tx.send(EmbedEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    vbox.upcast()
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
    SessionCreated(String),
    Token(String),
    Error(String),
    Done,
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
            DemoMode::Summarize => "Opening an llm.summarize session through the portal...",
            DemoMode::Translate => "Opening an llm.translate session through the portal...",
            DemoMode::Rephrase => "Opening an llm.rephrase session through the portal...",
            DemoMode::Classify => "Opening an llm.classify session through the portal...",
            DemoMode::Extract => "Opening an llm.extract session through the portal...",
            DemoMode::Analyze => "Opening an llm.analyze session through the portal...",
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
            DemoMode::Summarize => "llm.summarize",
            DemoMode::Translate => "llm.translate",
            DemoMode::Rephrase => "llm.rephrase",
            DemoMode::Classify => "llm.classify",
            DemoMode::Extract => "llm.extract",
            DemoMode::Analyze => "llm.analyze",
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

enum SpeechPhase {
    CreatingSession,
    LoadingModel,
    Transcribing,
}

impl SpeechPhase {
    fn title(&self) -> &'static str {
        match self {
            SpeechPhase::CreatingSession => "Creating ASR session",
            SpeechPhase::LoadingModel => "Loading ASR model",
            SpeechPhase::Transcribing => "Processing audio",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            SpeechPhase::CreatingSession => "Opening an ASR session through the portal...",
            SpeechPhase::LoadingModel => "Starting the local ASR container if it is cold...",
            SpeechPhase::Transcribing => "Sending recorded microphone audio to the ASR model...",
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
            EmbedPhase::CreatingSession => "Opening an llm.embed session through the portal...",
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

    if message.contains("huggingface.co") && message.contains("ggml-") {
        return "ASR model is missing from the assigned container image. The container tried to download a Whisper model from Hugging Face, but Aileron starts inference containers with networking disabled. Rebuild or assign an ASR image that has the Whisper model baked into /model.".to_string();
    }

    message.to_string()
}

#[cfg(test)]
mod tests {
    use super::concise_error;

    #[test]
    fn explains_missing_portal_systemd_unit() {
        let error = "org.freedesktop.DBus.Error.NameHasNoOwner: Could not activate remote peer 'org.freedesktop.impl.portal.desktop.aileron': activation request failed: unknown unit";

        assert_eq!(
            concise_error(error),
            "Aileron portal is not installed for D-Bus activation. Install systemd/aileron-portal.service to ~/.config/systemd/user/, run `systemctl --user daemon-reload`, then start `systemctl --user enable --now aileron-portal`."
        );
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
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

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
    let options = (512_i64, 0.2_f64, "default", "", "");

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

fn classify_guided(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

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
    let content: String = proxy.call(
        "RespondGuided",
        &(
            &session_id,
            &DemoMode::Classify.prompt(text),
            fields,
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
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

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

fn chat_stream(
    existing_session: Option<String>,
    messages: Vec<ChatMessage>,
    tx: std::sync::mpsc::Sender<ChatEvent>,
) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

    let call_conn = Connection::session()?;
    let signal_conn = Connection::session()?;

    let proxy = zbus::blocking::Proxy::new(&call_conn, BUS, PATH, IFACE)?;
    let sig_proxy = zbus::blocking::Proxy::new(&signal_conn, BUS, PATH, IFACE)?;

    let session_id = match existing_session {
        Some(id) => id,
        None => {
            let id: String = proxy.call(
                "CreateSession",
                &(
                    "org.aileron.Demo",
                    "llm.chat",
                    "You are a helpful local assistant. Be concise, accurate, and conversational.",
                ),
            )?;
            tx.send(ChatEvent::SessionCreated(id.clone()))?;
            id
        }
    };

    let mut token_iter = sig_proxy.receive_signal("TokenReceived")?;
    let dbus_messages: Vec<(String, String)> = messages
        .into_iter()
        .map(|message| (message.role, message.content))
        .collect();
    let options = (512_i64, 0.7_f64, "default", "", "");
    let _: () = proxy.call("StreamChat", &(&session_id, dbus_messages, options))?;

    for msg in &mut token_iter {
        let body = msg.body();
        let (sig_session, token, done): (String, String, bool) = body.deserialize()?;
        if sig_session != session_id {
            continue;
        }
        tx.send(ChatEvent::Token(token))?;
        if done {
            break;
        }
    }

    tx.send(ChatEvent::Done)?;
    Ok(())
}

fn end_chat_session(session_id: &str) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;
    let _: () = proxy.call("EndSession", &(session_id,))?;
    Ok(())
}

fn transcribe_recording(
    path: &PathBuf,
    use_case: &str,
    tx: std::sync::mpsc::Sender<SpeechEvent>,
) -> anyhow::Result<()> {
    use zbus::blocking::Connection;

    const BUS: &str = "org.freedesktop.impl.portal.desktop.aileron";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

    let audio = std::fs::read(path)?;
    if audio.is_empty() {
        anyhow::bail!("recording is empty");
    }

    let instructions = if use_case == "asr.translate" {
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
    // asr.translate reuses the Transcribe method; the daemon selects the
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
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

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
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

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
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

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
    const IFACE: &str = "org.freedesktop.impl.portal.AI";

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(EmbedEvent::Phase(EmbedPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            "llm.embed",
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
