use super::super::{EmbedEvent, embed_text, format_embedding, friendly_error};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Label, Orientation, ScrolledWindow, Spinner, TextBuffer, TextView};

pub(crate) fn build_page() -> gtk4::Widget {
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
