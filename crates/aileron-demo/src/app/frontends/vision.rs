use super::super::{
    VisionEvent, base64_encode, describe_image, format_segments, friendly_error, ocr_image,
    segment_image,
};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, FileDialog, Label, Orientation, ScrolledWindow, Spinner, TextBuffer,
    TextView,
};
use std::cell::RefCell;
use std::rc::Rc;

pub(crate) fn build_page() -> gtk4::Widget {
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
