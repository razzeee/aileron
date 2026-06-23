use super::super::{ToolEvent, friendly_error, run_character_tool_demo};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, Entry, Label, Orientation, ScrolledWindow, Spinner, TextBuffer, TextView,
};

pub(crate) fn build_page() -> gtk4::Widget {
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
