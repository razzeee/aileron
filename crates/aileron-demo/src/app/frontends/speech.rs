use super::super::{
    SpeechEvent, friendly_error, live_transcribe_recording, temp_audio_path, transcribe_recording,
};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, Entry, Label, Orientation, ScrolledWindow, Spinner, TextBuffer, TextView,
};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

enum RecordingMode {
    Manual,
    Live { stop: Arc<AtomicBool> },
}

struct Recording {
    child: Child,
    path: PathBuf,
    mode: RecordingMode,
}

#[derive(Clone)]
struct SpeechUi {
    recording: Rc<RefCell<Option<Recording>>>,
    record_button: Button,
    stop_button: Button,
    transcribe_button: Button,
    translate_button: Button,
    live_transcribe_button: Button,
    live_translate_button: Button,
    source_language_hint_entry: Entry,
    status_spinner: Spinner,
    status_title: Label,
    status_detail: Label,
    transcript_buffer: TextBuffer,
}

pub(crate) fn build_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Record microphone audio, transcribe it after capture, or stream provisional Speech results while recording.")
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
    let live_transcribe_button = Button::with_label("Live Transcribe");
    let live_translate_button = Button::with_label("Live Translate");
    button_row.append(&record_button);
    button_row.append(&stop_button);
    button_row.append(&transcribe_button);
    button_row.append(&translate_button);
    button_row.append(&live_transcribe_button);
    button_row.append(&live_translate_button);
    vbox.append(&button_row);

    let hint_row = Box::new(Orientation::Horizontal, 8);
    hint_row.append(
        &Label::builder()
            .label("Source language hint")
            .xalign(0.0)
            .build(),
    );
    let source_language_hint_entry = Entry::builder()
        .placeholder_text("Optional, e.g. de or en")
        .hexpand(true)
        .build();
    hint_row.append(&source_language_hint_entry);
    vbox.append(&hint_row);

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
        .label("Use Record for a saved capture, or Live Transcribe / Live Translate for provisional updates.")
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
    let ui = SpeechUi {
        recording,
        record_button,
        stop_button,
        transcribe_button,
        translate_button,
        live_transcribe_button,
        live_translate_button,
        source_language_hint_entry,
        status_spinner,
        status_title,
        status_detail,
        transcript_buffer,
    };

    {
        let ui_for_click = ui.clone();
        let last_audio = last_audio.clone();
        let record_button = ui.record_button.clone();
        record_button.connect_clicked(move |_| start_manual_recording(&ui_for_click, &last_audio));
    }

    {
        let ui_for_click = ui.clone();
        let last_audio = last_audio.clone();
        let stop_button = ui.stop_button.clone();
        stop_button.connect_clicked(move |_| stop_recording(&ui_for_click, &last_audio));
    }

    wire_asr_action(
        &ui,
        &last_audio,
        &ui.transcribe_button,
        "speech.transcribe",
        "transcribing",
    );
    wire_asr_action(
        &ui,
        &last_audio,
        &ui.translate_button,
        "speech.translate",
        "translating",
    );
    wire_live_action(
        &ui,
        &last_audio,
        &ui.live_transcribe_button,
        "speech.transcribe",
    );
    wire_live_action(
        &ui,
        &last_audio,
        &ui.live_translate_button,
        "speech.translate",
    );

    scrollable_page(&vbox)
}

fn start_manual_recording(ui: &SpeechUi, last_audio: &Rc<RefCell<Option<PathBuf>>>) {
    if ui.recording.borrow().is_some() {
        return;
    }

    let path = temp_audio_path();
    let child = match start_pw_record(&path) {
        Ok(child) => child,
        Err(e) => {
            ui.status_spinner.stop();
            ui.status_title.set_text("Recording unavailable");
            ui.status_detail
                .set_text(&format!("Could not start pw-record: {e}"));
            return;
        }
    };

    *ui.recording.borrow_mut() = Some(Recording {
        child,
        path,
        mode: RecordingMode::Manual,
    });
    *last_audio.borrow_mut() = None;
    ui.status_spinner.start();
    ui.status_title.set_text("Recording microphone");
    ui.status_detail
        .set_text("Speak now. Stop when you are ready to transcribe.");
    set_recording_controls(ui);
}

fn stop_recording(ui: &SpeechUi, last_audio: &Rc<RefCell<Option<PathBuf>>>) {
    let Some(mut current) = ui.recording.borrow_mut().take() else {
        return;
    };
    let live_stop = match &current.mode {
        RecordingMode::Live { stop } => Some(stop.clone()),
        RecordingMode::Manual => None,
    };

    let _ = current.child.kill();
    let _ = current.child.wait();
    *last_audio.borrow_mut() = Some(current.path);

    if let Some(stop) = live_stop {
        stop.store(true, Ordering::Release);
        ui.status_spinner.start();
        ui.status_title.set_text("Finalizing live transcript");
        ui.status_detail
            .set_text("Running a final streamed pass over the complete recording.");
        set_processing_controls(ui);
    } else {
        ui.status_spinner.stop();
        ui.status_title.set_text("Recording saved");
        ui.status_detail
            .set_text("Audio is ready. Stream transcribe or translate it through the portal.");
        set_idle_controls(ui, true);
    }
}

fn wire_asr_action(
    ui: &SpeechUi,
    last_audio: &Rc<RefCell<Option<PathBuf>>>,
    action_button: &Button,
    use_case: &'static str,
    verb: &'static str,
) {
    let ui = ui.clone();
    let last_audio = last_audio.clone();
    let action_button = action_button.clone();
    action_button.connect_clicked(move |_| {
        let Some(path) = last_audio.borrow().clone() else {
            ui.status_title.set_text("No recording");
            ui.status_detail
                .set_text(&format!("Record audio before {verb}."));
            return;
        };

        ui.transcript_buffer.set_text("");
        ui.status_spinner.start();
        ui.status_title.set_text("Creating Speech session");
        ui.status_detail.set_text(&format!(
            "Opening an {use_case} session through the portal..."
        ));
        set_processing_controls(&ui);
        let source_language_hint = ui.source_language_hint_entry.text().trim().to_string();

        let (tx, rx) = std::sync::mpsc::channel::<SpeechEvent>();
        start_speech_event_pump(ui.clone(), last_audio.clone(), rx);

        let error_tx = tx.clone();
        std::thread::spawn(move || {
            if let Err(e) = transcribe_recording(&path, use_case, &source_language_hint, tx) {
                eprintln!("[aileron-demo] speech error: {e}");
                let _ = error_tx.send(SpeechEvent::Error(friendly_error(&e)));
            }
        });
    });
}

fn wire_live_action(
    ui: &SpeechUi,
    last_audio: &Rc<RefCell<Option<PathBuf>>>,
    action_button: &Button,
    use_case: &'static str,
) {
    let ui = ui.clone();
    let last_audio = last_audio.clone();
    let action_button = action_button.clone();
    action_button.connect_clicked(move |_| {
        if ui.recording.borrow().is_some() {
            return;
        }

        let path = temp_audio_path();
        let child = match start_pw_record(&path) {
            Ok(child) => child,
            Err(e) => {
                ui.status_spinner.stop();
                ui.status_title.set_text("Recording unavailable");
                ui.status_detail
                    .set_text(&format!("Could not start pw-record: {e}"));
                return;
            }
        };
        let stop = Arc::new(AtomicBool::new(false));
        *ui.recording.borrow_mut() = Some(Recording {
            child,
            path: path.clone(),
            mode: RecordingMode::Live { stop: stop.clone() },
        });
        *last_audio.borrow_mut() = None;
        ui.transcript_buffer.set_text("");
        ui.status_spinner.start();
        ui.status_title.set_text("Live transcription running");
        ui.status_detail
            .set_text("Speak now. Interim text may change after the final pass.");
        set_recording_controls(&ui);
        let source_language_hint = ui.source_language_hint_entry.text().trim().to_string();

        let (tx, rx) = std::sync::mpsc::channel::<SpeechEvent>();
        start_speech_event_pump(ui.clone(), last_audio.clone(), rx);

        let error_tx = tx.clone();
        std::thread::spawn(move || {
            if let Err(e) =
                live_transcribe_recording(path, use_case, &source_language_hint, stop, tx)
            {
                eprintln!("[aileron-demo] live speech error: {e}");
                let _ = error_tx.send(SpeechEvent::Error(friendly_error(&e)));
            }
        });
    });
}

fn start_speech_event_pump(
    ui: SpeechUi,
    last_audio: Rc<RefCell<Option<PathBuf>>>,
    rx: std::sync::mpsc::Receiver<SpeechEvent>,
) {
    glib::timeout_add_local(Duration::from_millis(16), move || {
        loop {
            match rx.try_recv() {
                Ok(SpeechEvent::Phase(phase)) => {
                    ui.status_title.set_text(phase.title());
                    ui.status_detail.set_text(phase.detail());
                    ui.status_spinner.start();
                }
                Ok(SpeechEvent::Transcript(text)) => {
                    ui.transcript_buffer.set_text(&text);
                }
                Ok(SpeechEvent::AppendTranscript(text)) => {
                    let mut end = ui.transcript_buffer.end_iter();
                    ui.transcript_buffer.insert(&mut end, &text);
                }
                Ok(SpeechEvent::Error(message)) => {
                    abort_active_recording(&ui, &last_audio);
                    ui.status_spinner.stop();
                    ui.status_title.set_text("Speech request failed");
                    ui.status_detail.set_text(&message);
                    set_idle_controls(&ui, last_audio.borrow().is_some());
                    return glib::ControlFlow::Break;
                }
                Ok(SpeechEvent::Done) => {
                    ui.status_spinner.stop();
                    ui.status_title.set_text("Speech result complete");
                    ui.status_detail
                        .set_text("Speech streamed text through the portal.");
                    set_idle_controls(&ui, last_audio.borrow().is_some());
                    return glib::ControlFlow::Break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    abort_active_recording(&ui, &last_audio);
                    ui.status_spinner.stop();
                    ui.status_title.set_text("Speech request interrupted");
                    ui.status_detail
                        .set_text("The Speech response channel closed unexpectedly.");
                    set_idle_controls(&ui, last_audio.borrow().is_some());
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });
}

fn abort_active_recording(ui: &SpeechUi, last_audio: &Rc<RefCell<Option<PathBuf>>>) {
    let Some(mut current) = ui.recording.borrow_mut().take() else {
        return;
    };
    if let RecordingMode::Live { stop } = &current.mode {
        stop.store(true, Ordering::Release);
    }
    let _ = current.child.kill();
    let _ = current.child.wait();
    *last_audio.borrow_mut() = Some(current.path);
}

fn set_recording_controls(ui: &SpeechUi) {
    ui.record_button.set_sensitive(false);
    ui.stop_button.set_sensitive(true);
    ui.transcribe_button.set_sensitive(false);
    ui.translate_button.set_sensitive(false);
    ui.live_transcribe_button.set_sensitive(false);
    ui.live_translate_button.set_sensitive(false);
    ui.source_language_hint_entry.set_sensitive(false);
}

fn set_processing_controls(ui: &SpeechUi) {
    ui.record_button.set_sensitive(false);
    ui.stop_button.set_sensitive(false);
    ui.transcribe_button.set_sensitive(false);
    ui.translate_button.set_sensitive(false);
    ui.live_transcribe_button.set_sensitive(false);
    ui.live_translate_button.set_sensitive(false);
    ui.source_language_hint_entry.set_sensitive(false);
}

fn set_idle_controls(ui: &SpeechUi, has_audio: bool) {
    ui.record_button.set_sensitive(true);
    ui.stop_button.set_sensitive(false);
    ui.transcribe_button.set_sensitive(has_audio);
    ui.translate_button.set_sensitive(has_audio);
    ui.live_transcribe_button.set_sensitive(true);
    ui.live_translate_button.set_sensitive(true);
    ui.source_language_hint_entry.set_sensitive(true);
}

fn start_pw_record(path: &Path) -> std::io::Result<Child> {
    Command::new("pw-record")
        .args([
            "--raw",
            "--rate",
            "16000",
            "--channels",
            "1",
            "--format",
            "f32",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}
