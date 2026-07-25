use super::super::tts::CancellationToken;
use super::super::{
    DemoEvent, DemoMode, SpeechOutput, classify_guided, extract_guided, fetch_article_text,
    friendly_error, respond_text_task, summarize_streaming,
};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, CheckButton, DropDown, Entry, FileDialog, Label, Orientation,
    ScrolledWindow, Spinner, TextBuffer, TextView,
};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

pub(crate) fn build_page() -> gtk4::Widget {
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

    let speak_response = CheckButton::with_label("Speak streaming summaries");
    let keep_partial = CheckButton::with_label("Keep partial WAV when cancelled");
    keep_partial.set_sensitive(false);
    let choose_wav_button = Button::with_label("Choose WAV…");
    choose_wav_button.set_sensitive(false);
    let wav_label = Label::builder()
        .label("Playback only")
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::Middle)
        .build();
    let speech_row = Box::new(Orientation::Horizontal, 8);
    speech_row.append(&speak_response);
    speech_row.append(&choose_wav_button);
    speech_row.append(&wav_label);
    text_box.append(&speech_row);
    text_box.append(&keep_partial);

    let action_row = Box::new(Orientation::Horizontal, 8);
    let summarize_button = Button::builder()
        .label("Summarize")
        .css_classes(vec!["suggested-action"])
        .hexpand(true)
        .build();
    let cancel_button = Button::with_label("Cancel");
    cancel_button.set_sensitive(false);
    action_row.append(&summarize_button);
    action_row.append(&cancel_button);
    text_box.append(&action_row);

    let wav_path = Rc::new(RefCell::new(None::<PathBuf>));
    let active_cancel = Rc::new(RefCell::new(None::<CancellationToken>));

    {
        let choose_wav_button = choose_wav_button.clone();
        let keep_partial = keep_partial.clone();
        speak_response.connect_toggled(move |toggle| {
            choose_wav_button.set_sensitive(toggle.is_active());
            keep_partial.set_sensitive(toggle.is_active());
        });
    }

    {
        let wav_path = Rc::clone(&wav_path);
        let wav_label = wav_label.clone();
        choose_wav_button.connect_clicked(move |_| {
            let dialog = FileDialog::builder().title("Save spoken summary").build();
            let wav_path = Rc::clone(&wav_path);
            let wav_label = wav_label.clone();
            dialog.save(
                None::<&gtk4::Window>,
                None::<&gio::Cancellable>,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        wav_label.set_text("Selected destination is not a local file");
                        return;
                    };
                    wav_label.set_text(&path.display().to_string());
                    *wav_path.borrow_mut() = Some(path);
                },
            );
        });
    }

    {
        let summarize_button = summarize_button.clone();
        let speak_response = speak_response.clone();
        let choose_wav_button = choose_wav_button.clone();
        let keep_partial = keep_partial.clone();
        mode_dropdown.connect_selected_notify(move |dropdown| {
            let mode = DemoMode::from_index(dropdown.selected()).unwrap_or(DemoMode::Summarize);
            summarize_button.set_label(mode.ready_label());
            let can_speak = matches!(mode, DemoMode::Summarize);
            speak_response.set_sensitive(can_speak);
            choose_wav_button.set_sensitive(can_speak && speak_response.is_active());
            keep_partial.set_sensitive(can_speak && speak_response.is_active());
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

    {
        let active_cancel = Rc::clone(&active_cancel);
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        cancel_button.connect_clicked(move |button| {
            if let Some(cancel) = active_cancel.borrow().as_ref() {
                cancel.cancel();
                status_title.set_text("Cancelling");
                status_detail
                    .set_text("Stopping language generation, speech, playback, and recording…");
                button.set_sensitive(false);
            }
        });
    }

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
        let speak_response = speak_response.clone();
        let keep_partial = keep_partial.clone();
        let wav_path = Rc::clone(&wav_path);
        let active_cancel = Rc::clone(&active_cancel);
        let cancel_button_for_click = cancel_button.clone();
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
            let cancel = CancellationToken::default();
            *active_cancel.borrow_mut() = Some(cancel.clone());
            cancel_button_for_click.set_sensitive(matches!(mode, DemoMode::Summarize));
            let speech_output = (matches!(mode, DemoMode::Summarize) && speak_response.is_active())
                .then(|| SpeechOutput {
                    wav_path: wav_path.borrow().clone(),
                    keep_partial: keep_partial.is_active(),
                });

            // Channel: background thread sends tokens; glib main loop appends them.
            let (tx, rx) = std::sync::mpsc::channel::<DemoEvent>();

            let output_buffer_clone = output_buffer.clone();
            let summarize_button = summarize_button_for_click.clone();
            let status_spinner = status_spinner.clone();
            let status_title = status_title.clone();
            let status_detail = status_detail.clone();
            let active_cancel_for_poll = Rc::clone(&active_cancel);
            let cancel_button = cancel_button_for_click.clone();
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
                            if !saw_token {
                                saw_token = true;
                                status_title.set_text("Streaming guided JSON");
                                status_detail
                                    .set_text("Rendering guided snapshots as they arrive.");
                            }
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
                            cancel_button.set_sensitive(false);
                            active_cancel_for_poll.borrow_mut().take();
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
                                cancel_button.set_sensitive(false);
                                active_cancel_for_poll.borrow_mut().take();
                                return glib::ControlFlow::Break;
                            }
                            status_title.set_text(mode.complete_title());
                            status_detail.set_text(mode.complete_detail());
                            summarize_button.set_sensitive(true);
                            summarize_button.set_label(mode.ready_label());
                            cancel_button.set_sensitive(false);
                            active_cancel_for_poll.borrow_mut().take();
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
                            cancel_button.set_sensitive(false);
                            active_cancel_for_poll.borrow_mut().take();
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
                    DemoMode::Summarize => summarize_streaming(&text, tx, speech_output, cancel),
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
    scrollable_page(&text_box)
}
