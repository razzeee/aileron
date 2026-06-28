use super::super::{
    PredictionEvent, clear_failed_prediction_session, end_prediction_session, friendly_error,
    predict_inline_completion,
};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Entry, Label, Orientation, Spinner};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub(crate) fn build_page() -> gtk4::Widget {
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
                            status_detail.set_text(
                                "Requesting a short continuation through StreamPredictNext...",
                            );
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
                    Ok(PredictionEvent::Error {
                        seq,
                        message,
                        attempted_session,
                    }) => {
                        if seq == active_seq.get() {
                            status_spinner.stop();
                            status_title.set_text("Prediction failed");
                            status_detail.set_text(&message);
                            if let Some(id) = clear_failed_prediction_session(
                                &mut session_id.borrow_mut(),
                                attempted_session.as_deref(),
                            ) {
                                std::thread::spawn(move || {
                                    let _ = end_prediction_session(&id);
                                });
                            }
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
                let attempted_session = existing_session.clone();
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
                                attempted_session,
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
