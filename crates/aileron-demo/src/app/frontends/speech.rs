use super::super::{SpeechEvent, friendly_error, temp_audio_path, transcribe_recording};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Label, Orientation, ScrolledWindow, Spinner, TextBuffer, TextView};
use std::cell::RefCell;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

struct Recording {
    child: Child,
    path: PathBuf,
}

pub(crate) fn build_page() -> gtk4::Widget {
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
