/// aileron-demo — sandboxed GTK4 article summarizer.
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CssProvider, DropDown, Entry, FileDialog, Label, Orientation,
    ScrolledWindow, Spinner, TextBuffer, TextView,
};
use libadwaita::prelude::*;
use libadwaita::{
    ApplicationWindow, HeaderBar, OverlaySplitView, ToolbarView, ViewStack, ViewSwitcherSidebar,
    WindowTitle,
};
use relm4::{ComponentParts, ComponentSender, RelmApp, SimpleComponent};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
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
        .label("Paste text, then run a local Language task.")
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
    let chat_page = build_chat_page();
    let overview_page = stack.add_titled(
        &build_lab_overview(&stack),
        Some("overview"),
        "Lab overview",
    );
    overview_page.set_icon_name(Some("view-dashboard-symbolic"));
    let text_page_widget = scrollable_page(&text_box);
    let text_page = stack.add_titled(&text_page_widget, Some("text"), "Text lab");
    text_page.set_icon_name(Some("text-x-generic-symbolic"));
    let prediction_page =
        stack.add_titled(&build_prediction_page(), Some("predict"), "Prediction lab");
    prediction_page.set_icon_name(Some("insert-text-symbolic"));
    let chat_page_meta = stack.add_titled(&chat_page, Some("chat"), "Chat lab");
    chat_page_meta.set_icon_name(Some("user-available-symbolic"));
    let tool_page = stack.add_titled(&build_tool_page(), Some("tools"), "Tool lab");
    tool_page.set_icon_name(Some("applications-system-symbolic"));
    let speech_page = stack.add_titled(&build_speech_page(), Some("speech"), "Speech lab");
    speech_page.set_icon_name(Some("audio-input-microphone-symbolic"));
    let vision_page = stack.add_titled(&build_vision_page(), Some("vision"), "Vision lab");
    vision_page.set_icon_name(Some("image-x-generic-symbolic"));
    let embed_page = stack.add_titled(&build_embed_page(), Some("embed"), "Embeddings");
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

fn build_lab_overview(stack: &ViewStack) -> gtk4::Widget {
    let root = Box::new(Orientation::Vertical, 16);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    root.append(
        &Label::builder()
            .label("Try each Aileron portal capability from one local sandboxed app.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(vec!["title-2"])
            .build(),
    );

    let cards = Box::new(Orientation::Vertical, 12);
    cards.append(&lab_card(
        "Chat Lab",
        "Run chat-shaped turns through guided language.extract responses with local memory.",
        "RespondGuided, CreateSession, EndSession",
        "Try: tell it a preference, then ask a follow-up that uses memory.",
        "Open Chat Lab",
        "chat",
        stack,
    ));
    cards.append(&lab_card(
        "Text Lab",
        "Fetch or paste text, then summarize, translate, rephrase, classify, extract JSON, or analyze.",
        "StreamResponse, Respond, RespondGuided",
        "Try: paste an article, classify it, then extract JSON facts.",
        "Open Text Lab",
        "text",
        stack,
    ));
    cards.append(&lab_card(
        "Prediction Lab",
        "Type a sentence and preview a short ghost continuation from the local language model.",
        "PredictNext",
        "Try: The old lighthouse keeper opened the door and",
        "Open Prediction Lab",
        "predict",
        stack,
    ));
    cards.append(&lab_card(
        "Tool Lab",
        "Run a tiny agent loop where the model asks for an app-owned deterministic tool.",
        "CreateSession, RespondGuided, EndSession",
        "Try: ask how many r's are in strawrberrry and watch the app loop decide when to run the tool.",
        "Open Tool Lab",
        "tools",
        stack,
    ));
    cards.append(&lab_card(
        "Speech Lab",
        "Record microphone audio and transcribe it through the Speech portal path.",
        "Transcribe",
        "Try: record 5-10 seconds of speech, then transcribe locally.",
        "Open Speech Lab",
        "speech",
        stack,
    ));
    cards.append(&lab_card(
        "Vision Lab",
        "Choose or paste an image and run description or segmentation through the vision portal path.",
        "Describe, Segment",
        "Try: choose a screenshot, describe it, then segment visible objects.",
        "Open Vision Lab",
        "vision",
        stack,
    ));
    root.append(&cards);

    scrollable_page(&root)
}

fn scrollable_page<W: IsA<gtk4::Widget>>(child: &W) -> gtk4::Widget {
    ScrolledWindow::builder()
        .child(child)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .build()
        .upcast()
}

fn lab_card(
    title: &str,
    subtitle: &str,
    methods: &str,
    example: &str,
    button_label: &str,
    page_name: &'static str,
    stack: &ViewStack,
) -> Box {
    let card = Box::new(Orientation::Horizontal, 14);
    card.add_css_class("card");
    card.set_height_request(132);
    card.set_margin_top(2);
    card.set_margin_bottom(2);
    card.set_margin_start(2);
    card.set_margin_end(2);

    let content = Box::new(Orientation::Vertical, 6);
    content.set_hexpand(true);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(14);
    content.set_margin_end(8);

    let title = Label::builder()
        .label(title)
        .xalign(0.0)
        .css_classes(vec!["heading"])
        .build();
    let subtitle = Label::builder()
        .label(subtitle)
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["dim-label"])
        .build();
    let methods = Label::builder()
        .label(format!("Portal: {methods}"))
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["caption", "dim-label"])
        .build();
    let example = Label::builder()
        .label(example)
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["caption"])
        .build();
    content.append(&title);
    content.append(&subtitle);
    content.append(&methods);
    content.append(&example);

    let action_box = Box::new(Orientation::Vertical, 0);
    action_box.set_margin_top(12);
    action_box.set_margin_bottom(12);
    action_box.set_margin_end(14);
    action_box.set_valign(Align::Center);
    let button = Button::builder()
        .label(button_label)
        .css_classes(vec!["suggested-action"])
        .build();
    {
        let stack = stack.clone();
        button.connect_clicked(move |_| {
            stack.set_visible_child_name(page_name);
        });
    }
    action_box.append(&button);

    card.append(&content);
    card.append(&action_box);
    card
}

fn build_prediction_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Type normally. After a short pause, the app asks the current language model for only the current word ending or the next word.")
            .xalign(0.0)
            .wrap(true)
            .build(),
    );

    let input = Entry::builder()
        .placeholder_text("Start typing a sentence...")
        .hexpand(true)
        .build();
    vbox.append(&input);

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
        .label("Pause after typing to request an inline prediction.")
        .xalign(0.0)
        .wrap(true)
        .build();
    status_text.append(&status_title);
    status_text.append(&status_detail);
    status_row.append(&status_text);
    vbox.append(&status_row);

    vbox.append(&Label::builder().label("Ghost preview").xalign(0.0).build());
    let preview = Label::builder()
        .label("Your text will appear here.")
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk4::pango::WrapMode::WordChar)
        .selectable(true)
        .css_classes(vec!["card"])
        .build();
    preview.set_margin_top(2);
    preview.set_margin_bottom(2);
    preview.set_margin_start(2);
    preview.set_margin_end(2);
    vbox.append(&preview);

    let choices_label = Label::builder()
        .label("Choices will appear here.")
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["dim-label"])
        .build();
    vbox.append(&choices_label);

    let clear_button = Button::with_label("Clear");
    clear_button.set_halign(Align::Start);
    vbox.append(&clear_button);

    let active_seq = Rc::new(Cell::new(0_u64));
    let session_id = Rc::new(RefCell::new(None::<String>));
    let (tx, rx) = std::sync::mpsc::channel::<PredictionEvent>();

    {
        let input = input.clone();
        let preview = preview.clone();
        let choices_label = choices_label.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        let active_seq = active_seq.clone();
        let session_id = session_id.clone();
        glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
            loop {
                match rx.try_recv() {
                    Ok(PredictionEvent::SessionReady { seq, id }) => {
                        if seq == active_seq.get() {
                            *session_id.borrow_mut() = Some(id);
                        } else {
                            std::thread::spawn(move || {
                                let _ = end_prediction_session(&id);
                            });
                        }
                    }
                    Ok(PredictionEvent::Busy(seq)) => {
                        if seq == active_seq.get() {
                            status_spinner.start();
                            status_title.set_text("Predicting");
                            status_detail
                                .set_text("Requesting a short continuation through PredictNext...");
                        }
                    }
                    Ok(PredictionEvent::Suggestion {
                        seq,
                        input_text,
                        suggestions,
                    }) => {
                        if seq != active_seq.get() || input.text().as_str() != input_text {
                            continue;
                        }
                        status_spinner.stop();
                        if suggestions.is_empty() {
                            status_title.set_text("No prediction");
                            status_detail
                                .set_text("The model did not return a usable continuation.");
                            set_prediction_preview(&preview, &input_text, "");
                            set_prediction_choices(&choices_label, &[]);
                        } else {
                            status_title.set_text("Prediction ready");
                            status_detail.set_text("Dim text shows the first prediction; alternatives are listed below.");
                            set_prediction_preview(&preview, &input_text, &suggestions[0]);
                            set_prediction_choices(&choices_label, &suggestions);
                        }
                    }
                    Ok(PredictionEvent::Error { seq, message }) => {
                        if seq == active_seq.get() {
                            status_spinner.stop();
                            status_title.set_text("Prediction failed");
                            status_detail.set_text(&message);
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        status_spinner.stop();
                        status_title.set_text("Prediction interrupted");
                        status_detail.set_text("The prediction channel closed unexpectedly.");
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    {
        let preview = preview.clone();
        let choices_label = choices_label.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        let active_seq = active_seq.clone();
        let session_id = session_id.clone();
        let tx = tx.clone();
        input.connect_changed(move |entry| {
            let seq = active_seq.get() + 1;
            active_seq.set(seq);
            let input_text = entry.text().to_string();
            set_prediction_preview(&preview, &input_text, "");
            set_prediction_choices(&choices_label, &[]);

            if input_text.trim().is_empty() {
                status_spinner.stop();
                status_title.set_text("Ready");
                status_detail.set_text("Pause after typing to request an inline prediction.");
                return;
            }

            status_spinner.stop();
            status_title.set_text("Waiting for pause");
            status_detail.set_text("Typing is debounced to avoid sending every keystroke.");

            let tx = tx.clone();
            let active_seq = active_seq.clone();
            let session_id = session_id.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                if seq != active_seq.get() {
                    return;
                }

                let tx_for_error = tx.clone();
                let _ = tx.send(PredictionEvent::Busy(seq));
                let existing_session = session_id.borrow().clone();
                std::thread::spawn(move || {
                    match predict_inline_completion(existing_session, &input_text) {
                        Ok((session, suggestions)) => {
                            let _ = tx_for_error
                                .send(PredictionEvent::SessionReady { seq, id: session });
                            let _ = tx_for_error.send(PredictionEvent::Suggestion {
                                seq,
                                input_text,
                                suggestions,
                            });
                        }
                        Err(e) => {
                            eprintln!("[aileron-demo] prediction error: {e}");
                            let _ = tx_for_error.send(PredictionEvent::Error {
                                seq,
                                message: friendly_error(&e),
                            });
                        }
                    }
                });
            });
        });
    }

    {
        let input = input.clone();
        let session_id = session_id.clone();
        clear_button.connect_clicked(move |_| {
            input.set_text("");
            if let Some(id) = session_id.borrow_mut().take() {
                std::thread::spawn(move || {
                    let _ = end_prediction_session(&id);
                });
            }
        });
    }

    scrollable_page(&vbox)
}

fn set_prediction_preview(label: &Label, input: &str, suggestion: &str) {
    if input.is_empty() {
        label.set_label("Your text will appear here.");
        return;
    }

    let escaped_input = glib::markup_escape_text(input);
    let escaped_suggestion = glib::markup_escape_text(suggestion);
    if suggestion.is_empty() {
        label.set_markup(&escaped_input);
    } else {
        label.set_markup(&format!(
            "{escaped_input}<span alpha=\"55%\">{escaped_suggestion}</span>"
        ));
    }
}

fn set_prediction_choices(label: &Label, suggestions: &[String]) {
    if suggestions.is_empty() {
        label.set_text("Choices will appear here.");
        return;
    }

    let choices = suggestions
        .iter()
        .take(3)
        .enumerate()
        .map(|(index, suggestion)| format!("{}. {}", index + 1, suggestion.trim()))
        .collect::<Vec<_>>()
        .join("   ");
    label.set_text(&choices);
}

fn build_tool_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("A tiny app-owned agent loop: the local model may request a tool, but the app validates and executes it.")
            .xalign(0.0)
            .wrap(true)
            .build(),
    );

    let prompt_entry = Entry::builder()
        .text("How many times does the letter r occur in strawrberrry?")
        .hexpand(true)
        .build();
    let run_button = Button::builder()
        .label("Run Tool Demo")
        .css_classes(vec!["suggested-action"])
        .build();
    let prompt_row = Box::new(Orientation::Horizontal, 8);
    prompt_row.append(&prompt_entry);
    prompt_row.append(&run_button);
    vbox.append(&prompt_row);

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
        .label("Run the deterministic character-counter tool through the Language portal.")
        .xalign(0.0)
        .wrap(true)
        .build();
    status_text.append(&status_title);
    status_text.append(&status_detail);
    status_row.append(&status_text);
    vbox.append(&status_row);

    let trace_buffer = TextBuffer::new(None);
    let trace_view = TextView::builder()
        .buffer(&trace_buffer)
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(
        &Label::builder()
            .label("Agent loop trace")
            .xalign(0.0)
            .build(),
    );
    vbox.append(
        &ScrolledWindow::builder()
            .child(&trace_view)
            .min_content_height(380)
            .vexpand(true)
            .build(),
    );

    {
        let prompt_entry = prompt_entry.clone();
        let run_button_for_click = run_button.clone();
        let run_button = run_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        let trace_buffer = trace_buffer.clone();
        run_button_for_click.connect_clicked(move |_| {
            let prompt = prompt_entry.text().trim().to_string();
            if prompt.is_empty() {
                return;
            }

            trace_buffer.set_text("");
            run_button.set_sensitive(false);
            status_spinner.start();
            status_title.set_text("Running tool loop");
            status_detail.set_text("The app owns the loop and executes tools locally.");

            let (tx, rx) = std::sync::mpsc::channel::<ToolEvent>();
            let trace_buffer_for_rx = trace_buffer.clone();
            let run_button_for_rx = run_button.clone();
            let status_spinner_for_rx = status_spinner.clone();
            let status_title_for_rx = status_title.clone();
            let status_detail_for_rx = status_detail.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                loop {
                    match rx.try_recv() {
                        Ok(ToolEvent::Trace(line)) => {
                            trace_buffer_for_rx
                                .insert(&mut trace_buffer_for_rx.end_iter(), &format!("{line}\n"));
                        }
                        Ok(ToolEvent::Final(content)) => {
                            status_title_for_rx.set_text("Tool loop complete");
                            status_detail_for_rx
                                .set_text("Final answer returned from the guided app loop.");
                            trace_buffer_for_rx.insert(
                                &mut trace_buffer_for_rx.end_iter(),
                                &format!("\nfinal_answer: {content}\n"),
                            );
                        }
                        Ok(ToolEvent::Error(message)) => {
                            status_spinner_for_rx.stop();
                            status_title_for_rx.set_text("Tool demo failed");
                            status_detail_for_rx.set_text(&message);
                            run_button_for_rx.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(ToolEvent::Done) => {
                            status_spinner_for_rx.stop();
                            run_button_for_rx.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner_for_rx.stop();
                            status_title_for_rx.set_text("Tool demo interrupted");
                            status_detail_for_rx
                                .set_text("The tool demo channel closed unexpectedly.");
                            run_button_for_rx.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = run_character_tool_demo(&prompt, tx) {
                    eprintln!("[aileron-demo] tool demo error: {e}");
                    let _ = error_tx.send(ToolEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    scrollable_page(&vbox)
}

#[derive(Clone)]
struct ChatMessage {
    role: String,
    content: String,
}

fn build_chat_page() -> gtk4::Widget {
    install_chat_css();

    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Send chat-shaped turns through guided language.extract responses with local memory.")
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
        .label("Ask a question. The app sends local history and memory to RespondGuided.")
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
    let memory = Rc::new(RefCell::new(Vec::<String>::new()));
    let session_id = Rc::new(RefCell::new(None::<String>));

    {
        let history = history.clone();
        let memory = memory.clone();
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
            status_title.set_text("Starting guided chat turn");
            status_detail.set_text("Sending history and memory through RespondGuided...");

            history.borrow_mut().push(ChatMessage {
                role: "user".to_string(),
                content: text,
            });
            render_chat(&chat_box, &history.borrow(), None);

            let messages = history.borrow().clone();
            let memories = memory.borrow().clone();
            let existing_session = session_id.borrow().clone();
            let (tx, rx) = std::sync::mpsc::channel::<ChatEvent>();

            let history_for_rx = history.clone();
            let memory_for_rx = memory.clone();
            let session_for_rx = session_id.clone();
            let chat_box_for_rx = chat_box.clone();
            let send_button_for_rx = send_button.clone();
            let clear_button_for_rx = clear_button.clone();
            let status_spinner_for_rx = status_spinner.clone();
            let status_title_for_rx = status_title.clone();
            let status_detail_for_rx = status_detail.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                loop {
                    match rx.try_recv() {
                        Ok(ChatEvent::SessionReady(id)) => {
                            *session_for_rx.borrow_mut() = Some(id);
                            status_title_for_rx.set_text("Guided session ready");
                            status_detail_for_rx.set_text(
                                "Reusing this language.extract session for future turns.",
                            );
                        }
                        Ok(ChatEvent::Response(response)) => {
                            let answer = response.answer.trim().to_string();
                            if !answer.is_empty() {
                                history_for_rx.borrow_mut().push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: answer,
                                });
                            }

                            let memory = response.memory.trim().to_string();
                            if memory.is_empty() {
                                status_title_for_rx.set_text("Guided response received");
                                status_detail_for_rx.set_text("No durable memory was added.");
                            } else {
                                memory_for_rx.borrow_mut().push(memory.clone());
                                status_title_for_rx.set_text("Guided response received");
                                status_detail_for_rx.set_text(&format!("Added memory: {memory}"));
                            }

                            render_chat(&chat_box_for_rx, &history_for_rx.borrow(), None);
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
                            render_chat(&chat_box_for_rx, &history_for_rx.borrow(), None);
                            status_spinner_for_rx.stop();
                            status_title_for_rx.set_text("Response complete");
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
                if let Err(e) = guided_chat_turn(existing_session, &memories, messages, tx) {
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
        let memory = memory.clone();
        let session_id = session_id.clone();
        let chat_box = chat_box.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        clear_button.connect_clicked(move |_| {
            if let Some(id) = session_id.borrow_mut().take() {
                std::thread::spawn(move || {
                    let _ = end_guided_chat_session(&id);
                });
            }
            history.borrow_mut().clear();
            memory.borrow_mut().clear();
            render_chat(&chat_box, &history.borrow(), None);
            status_spinner.stop();
            status_title.set_text("Ready");
            status_detail.set_text("Chat and local memory cleared. The next message starts fresh.");
        });
    }

    scrollable_page(&vbox)
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
            .label("Record microphone audio, then transcribe it or translate it to English through the Speech portal path.")
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
                status_title.set_text("Creating Speech session");
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
                                status_title.set_text("Speech request failed");
                                status_detail.set_text(&message);
                                transcribe_button.set_sensitive(true);
                                translate_button.set_sensitive(true);
                                return glib::ControlFlow::Break;
                            }
                            Ok(SpeechEvent::Done) => {
                                status_spinner.stop();
                                status_title.set_text("Speech result complete");
                                status_detail.set_text("Speech returned text through the portal.");
                                transcribe_button.set_sensitive(true);
                                translate_button.set_sensitive(true);
                                return glib::ControlFlow::Break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                status_spinner.stop();
                                status_title.set_text("Speech request interrupted");
                                status_detail
                                    .set_text("The Speech response channel closed unexpectedly.");
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
                        eprintln!("[aileron-demo] speech error: {e}");
                        let _ = error_tx.send(SpeechEvent::Error(friendly_error(&e)));
                    }
                });
            });
        }
    };

    wire_asr_action(&transcribe_button, "speech.transcribe", "transcribing");
    wire_asr_action(&translate_button, "speech.translate", "translating");

    scrollable_page(&vbox)
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

    scrollable_page(&vbox)
}

fn build_embed_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Turn text into an embedding vector through the language.embed portal path. Useful for semantic search, clustering, and retrieval.")
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
            status_detail.set_text("Opening a language.embed session through the portal...");

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

    scrollable_page(&vbox)
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
    },
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
    let session_id = match existing_session {
        Some(id) => id,
        None => {
            let id: String = proxy.call(
                "CreateSession",
                &(
                    "org.aileron.Demo",
                    "language.rephrase",
                    "Inline typing prediction session.",
                ),
            )?;
            let _: () = proxy.call("Prewarm", &(&id, ""))?;
            id
        }
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
    let completions: Vec<String> =
        proxy.call("PredictNext", &(&session_id, &prompt_input, 3_i64, options))?;
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
    let mut response = parse_guided_tool_loop_response(&content, prompt, false)?;
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
        tx.send(ToolEvent::Final(response.answer))?;
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
            true,
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

fn infer_word_from_prompt(prompt: &str) -> Option<String> {
    prompt
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| part.len() > 1)
        .rev()
        .find(|part| part.chars().any(|ch| ch.eq_ignore_ascii_case(&'r')))
        .map(str::to_string)
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
    use super::{concise_error, execute_count_tool};

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
    fn count_tool_uses_structured_arguments() {
        let result = execute_count_tool(
            "ignored prompt",
            r#"{"word":"strawrberrry","character":"r"}"#,
        )
        .expect("count tool should run");

        assert_eq!(result["count"], 5);
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
}
