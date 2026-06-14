/// aileron-demo — sandboxed GTK4 article summarizer.
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CheckButton, Entry, FileDialog, Label, Orientation, ScrolledWindow,
    Spinner, Stack, StackSwitcher, TextBuffer, TextView,
};
use libadwaita::{Application, ApplicationWindow, HeaderBar, ToolbarView};
use std::cell::RefCell;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

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
    let streaming_mode = CheckButton::with_label("Streaming Summary");
    streaming_mode.set_active(true);
    let guided_mode = CheckButton::with_label("Guided JSON");
    guided_mode.set_group(Some(&streaming_mode));
    mode_row.append(&streaming_mode);
    mode_row.append(&guided_mode);
    text_box.append(&mode_row);

    let summarize_button = Button::builder()
        .label("Summarize")
        .css_classes(vec!["suggested-action"])
        .build();
    text_box.append(&summarize_button);

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
    text_box.append(&Label::builder().label("Summary").xalign(0.0).build());
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
                    let _ = error_tx.send(DemoEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    // ── Window ────────────────────────────────────────────────────────────────
    let stack = Stack::new();
    stack.add_titled(&text_box, Some("text"), "Text");
    stack.add_titled(&build_speech_page(), Some("speech"), "Speech");
    stack.add_titled(&build_vision_page(), Some("vision"), "Vision");

    let switcher = StackSwitcher::new();
    switcher.set_stack(Some(&stack));

    let header = HeaderBar::new();
    header.set_title_widget(Some(&switcher));
    let toolbar_view = ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&stack));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aileron Demo — Article Summarizer")
        .default_width(700)
        .default_height(700)
        .content(&toolbar_view)
        .build();

    window.present();
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
            .label("Record microphone audio, then send it through the ASR portal path.")
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
    button_row.append(&record_button);
    button_row.append(&stop_button);
    button_row.append(&transcribe_button);
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
        });
    }

    {
        let recording = recording.clone();
        let last_audio = last_audio.clone();
        let record_button = record_button.clone();
        let stop_button_for_click = stop_button.clone();
        let stop_button = stop_button.clone();
        let transcribe_button = transcribe_button.clone();
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
            status_detail.set_text("Audio is ready. Transcribe it through the portal.");
            record_button.set_sensitive(true);
            stop_button.set_sensitive(false);
            transcribe_button.set_sensitive(true);
        });
    }

    {
        let last_audio = last_audio.clone();
        let transcript_buffer = transcript_buffer.clone();
        let transcribe_button_for_click = transcribe_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        transcribe_button.connect_clicked(move |_| {
            let Some(path) = last_audio.borrow().clone() else {
                status_title.set_text("No recording");
                status_detail.set_text("Record audio before transcribing.");
                return;
            };

            transcript_buffer.set_text("");
            transcribe_button_for_click.set_sensitive(false);
            status_spinner.start();
            status_title.set_text("Creating ASR session");
            status_detail.set_text("Opening an asr.transcribe session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<SpeechEvent>();
            let transcript_buffer = transcript_buffer.clone();
            let transcribe_button = transcribe_button_for_click.clone();
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
                            status_title.set_text("Transcription failed");
                            status_detail.set_text(&message);
                            transcribe_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(SpeechEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Transcript complete");
                            status_detail.set_text("ASR returned text through the portal.");
                            transcribe_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Transcription interrupted");
                            status_detail.set_text("The ASR response channel closed unexpectedly.");
                            transcribe_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = transcribe_recording(&path, tx) {
                    eprintln!("[aileron-demo] transcribe error: {e}");
                    let _ = error_tx.send(SpeechEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

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
            .label("Describe an image through the vision portal path. Select a PNG/JPEG file or paste base64 image bytes.")
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
    button_row.append(&choose_button);
    button_row.append(&describe_button);
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
            .min_content_height(260)
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
                                "Use Describe Image to send it through the vision portal.",
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
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.describe session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let description_buffer = description_buffer.clone();
            let describe_button = describe_button_for_click.clone();
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
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Description failed");
                            status_detail.set_text(&message);
                            describe_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Description complete");
                            status_detail
                                .set_text("Vision returned a description through the portal.");
                            describe_button.set_sensitive(true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Description interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            describe_button.set_sensitive(true);
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
            SpeechPhase::Transcribing => "Transcribing audio",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            SpeechPhase::CreatingSession => {
                "Opening an asr.transcribe session through the portal..."
            }
            SpeechPhase::LoadingModel => "Starting the local ASR container if it is cold...",
            SpeechPhase::Transcribing => "Sending recorded microphone audio to the ASR model...",
        }
    }
}

enum VisionEvent {
    Phase(VisionPhase),
    Description(String),
    Error(String),
    Done,
}

enum VisionPhase {
    CreatingSession,
    LoadingModel,
    Describing,
}

impl VisionPhase {
    fn title(&self) -> &'static str {
        match self {
            VisionPhase::CreatingSession => "Creating vision session",
            VisionPhase::LoadingModel => "Loading vision model",
            VisionPhase::Describing => "Describing image",
        }
    }

    fn detail(&self) -> &'static str {
        match self {
            VisionPhase::CreatingSession => {
                "Opening a vision.describe session through the portal..."
            }
            VisionPhase::LoadingModel => "Starting the local vision container if it is cold...",
            VisionPhase::Describing => "Sending image bytes to the vision model...",
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
    if message.contains("huggingface.co") && message.contains("ggml-") {
        return "ASR model is missing from the assigned container image. The container tried to download a Whisper model from Hugging Face, but Aileron starts inference containers with networking disabled. Rebuild or assign an ASR image that has the Whisper model baked into /model.".to_string();
    }

    message.to_string()
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
            "llm.summarize",
            "You summarize user-provided text clearly and concisely.",
        ),
    )?;

    drop(loading_thread);
    tx.send(DemoEvent::Phase(DemoPhase::WaitingForModel))?;

    let prompt = format!(
        "Summarize the following article in 3-5 sentences. Return only the summary. Do not repeat or answer the instruction/question:\n\n{}",
        &text[..text.len().min(8192)]
    );

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

fn summarize_guided(text: &str, tx: std::sync::mpsc::Sender<DemoEvent>) -> anyhow::Result<()> {
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
            "llm.analyze",
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

fn transcribe_recording(
    path: &PathBuf,
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

    let conn = Connection::session()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS, PATH, IFACE)?;

    tx.send(SpeechEvent::Phase(SpeechPhase::CreatingSession))?;
    let session_id: String = proxy.call(
        "CreateSession",
        &(
            "org.aileron.Demo",
            "asr.transcribe",
            "Transcribe the provided audio accurately.",
        ),
    )?;

    tx.send(SpeechEvent::Phase(SpeechPhase::LoadingModel))?;
    tx.send(SpeechEvent::Phase(SpeechPhase::Transcribing))?;
    let audio_b64 = base64_encode(&audio);
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
