use super::super::{
    ChatEvent, ChatMessage, end_guided_chat_session, friendly_error, guided_chat_turn,
};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, CssProvider, Entry, Label, Orientation, ScrolledWindow, Spinner};
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) fn build_page() -> gtk4::Widget {
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
        .label("Ask a question. The app sends local history and memory to StreamRespondGuided.")
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
            status_detail.set_text("Sending history and memory through StreamRespondGuided...");

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
