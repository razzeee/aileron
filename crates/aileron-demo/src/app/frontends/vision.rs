use super::super::{
    VisionDepthMapDbus, VisionDetectionDbus, VisionEvent, VisionMaskDbus, VisionPointPromptDbus,
    depth_image, describe_image, detect_image, format_depth, format_detections, format_masks,
    friendly_error, ocr_image, segment_image,
};
use super::scrollable_page;
use base64::Engine as _;
use gtk4::prelude::*;
use gtk4::{
    Align, AspectFrame, Box, Button, DrawingArea, FileDialog, GestureClick, Label, Orientation,
    ScrolledWindow, Spinner, TextBuffer, TextView, cairo, gdk,
};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, Eq, PartialEq)]
enum PromptEditMode {
    Add,
    Remove,
}

pub(crate) fn build_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("Describe, extract text from, or segment an image through the vision portal path. Select a PNG/JPEG file.")
            .xalign(0.0)
            .wrap(true)
            .build(),
    );

    let selected_image = Rc::new(RefCell::new(None::<Vec<u8>>));
    let selected_image_size = Rc::new(RefCell::new(None::<(i32, i32)>));
    let selected_image_surface = Rc::new(RefCell::new(None::<cairo::ImageSurface>));
    let detections_overlay = Rc::new(RefCell::new(Vec::<VisionDetectionDbus>::new()));
    let masks_overlay = Rc::new(RefCell::new(Vec::<VisionMaskDbus>::new()));
    let mask_surfaces = Rc::new(RefCell::new(Vec::<Option<cairo::ImageSurface>>::new()));
    let depth_map = Rc::new(RefCell::new(None::<VisionDepthMapDbus>));
    let segment_prompts = Rc::new(RefCell::new(Vec::<VisionPointPromptDbus>::new()));
    let prompt_edit_mode = Rc::new(RefCell::new(PromptEditMode::Add));

    let button_row = Box::new(Orientation::Horizontal, 8);
    let choose_button = Button::with_label("Choose Image");
    let describe_button = Button::builder()
        .label("Describe Image")
        .css_classes(vec!["suggested-action"])
        .build();
    let detect_button = Button::with_label("Detect Objects");
    let segment_button = Button::with_label("Segment Prompted Object");
    let depth_button = Button::with_label("Estimate Depth");
    let ocr_button = Button::with_label("Extract Text");
    let action_buttons = Rc::new(vec![
        describe_button.clone(),
        ocr_button.clone(),
        detect_button.clone(),
        segment_button.clone(),
        depth_button.clone(),
    ]);
    button_row.append(&choose_button);
    button_row.append(&describe_button);
    button_row.append(&ocr_button);
    button_row.append(&detect_button);
    button_row.append(&segment_button);
    button_row.append(&depth_button);
    vbox.append(&button_row);

    let selected_label = Label::builder()
        .label("No file selected. Choose an image.")
        .xalign(0.0)
        .wrap(true)
        .build();
    vbox.append(&selected_label);

    let prompt_mode_row = Box::new(Orientation::Horizontal, 8);
    let prompt_mode_label = Label::builder()
        .label("Prompt mode: Add points")
        .xalign(0.0)
        .build();
    let add_prompt_button = Button::with_label("Add points");
    let remove_prompt_button = Button::with_label("Remove points");
    prompt_mode_row.append(&prompt_mode_label);
    prompt_mode_row.append(&add_prompt_button);
    prompt_mode_row.append(&remove_prompt_button);
    vbox.append(&prompt_mode_row);
    {
        let prompt_edit_mode = prompt_edit_mode.clone();
        let prompt_mode_label = prompt_mode_label.clone();
        add_prompt_button.connect_clicked(move |_| {
            *prompt_edit_mode.borrow_mut() = PromptEditMode::Add;
            prompt_mode_label.set_text("Prompt mode: Add points");
        });
    }
    {
        let prompt_edit_mode = prompt_edit_mode.clone();
        let prompt_mode_label = prompt_mode_label.clone();
        remove_prompt_button.connect_clicked(move |_| {
            *prompt_edit_mode.borrow_mut() = PromptEditMode::Remove;
            prompt_mode_label.set_text("Prompt mode: Remove points");
        });
    }

    let overlay_area = DrawingArea::builder()
        .hexpand(true)
        .vexpand(true)
        .width_request(560)
        .height_request(320)
        .build();
    {
        let detections_overlay = detections_overlay.clone();
        let masks_overlay = masks_overlay.clone();
        let mask_surfaces = mask_surfaces.clone();
        let selected_image_surface = selected_image_surface.clone();
        let selected_image_size = selected_image_size.clone();
        let segment_prompts = segment_prompts.clone();
        overlay_area.set_draw_func(move |_area, cr, width, height| {
            let (offset_x, offset_y, draw_width, draw_height) =
                fitted_image_rect(width as f64, height as f64, *selected_image_size.borrow());
            if let Some(surface) = selected_image_surface.borrow().as_ref() {
                let _ = cr.save();
                cr.translate(offset_x, offset_y);
                cr.scale(
                    draw_width / surface.width().max(1) as f64,
                    draw_height / surface.height().max(1) as f64,
                );
                let _ = cr.set_source_surface(surface, 0.0, 0.0);
                let _ = cr.paint();
                let _ = cr.restore();
            }
            cr.set_line_width(2.0);
            for (index, detection) in detections_overlay.borrow().iter().enumerate() {
                let (red, green, blue) = detection_color(index);
                cr.set_source_rgba(red, green, blue, 0.9);
                cr.rectangle(
                    offset_x + detection.x * draw_width,
                    offset_y + detection.y * draw_height,
                    detection.width * draw_width,
                    detection.height * draw_height,
                );
                let _ = cr.stroke();
            }
            let masks = masks_overlay.borrow();
            let decoded_masks = mask_surfaces.borrow();
            for (index, mask) in masks.iter().enumerate() {
                if let Some(Some(surface)) = decoded_masks.get(index) {
                    let _ = cr.save();
                    let full_image_mask =
                        selected_image_size
                            .borrow()
                            .is_some_and(|(image_width, image_height)| {
                                surface.width() == image_width && surface.height() == image_height
                            });
                    if full_image_mask {
                        cr.translate(offset_x, offset_y);
                        cr.scale(
                            draw_width / surface.width().max(1) as f64,
                            draw_height / surface.height().max(1) as f64,
                        );
                    } else {
                        cr.translate(
                            offset_x + mask.x * draw_width,
                            offset_y + mask.y * draw_height,
                        );
                        cr.scale(
                            mask.width * draw_width / surface.width().max(1) as f64,
                            mask.height * draw_height / surface.height().max(1) as f64,
                        );
                    }
                    let _ = cr.set_source_surface(surface, 0.0, 0.0);
                    let _ = cr.paint();
                    let _ = cr.restore();
                } else {
                    cr.set_source_rgba(0.7, 0.25, 1.0, 0.25);
                    cr.rectangle(
                        offset_x + mask.x * draw_width,
                        offset_y + mask.y * draw_height,
                        mask.width * draw_width,
                        mask.height * draw_height,
                    );
                    let _ = cr.fill_preserve();
                    cr.set_source_rgba(0.7, 0.25, 1.0, 0.85);
                    let _ = cr.stroke();
                }
            }
            for prompt in segment_prompts.borrow().iter() {
                let x = offset_x + prompt.x * draw_width;
                let y = offset_y + prompt.y * draw_height;
                cr.set_line_width(2.0);
                cr.set_source_rgba(1.0, 0.75, 0.1, 0.95);
                cr.arc(x, y, 7.0, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
                cr.move_to(x - 11.0, y);
                cr.line_to(x + 11.0, y);
                cr.move_to(x, y - 11.0);
                cr.line_to(x, y + 11.0);
                let _ = cr.stroke();
            }
        });
    }
    {
        let selected_image_size = selected_image_size.clone();
        let segment_prompts = segment_prompts.clone();
        let prompt_edit_mode = prompt_edit_mode.clone();
        let click_area = overlay_area.clone();
        let draw_area = overlay_area.clone();
        let click = GestureClick::new();
        click.connect_pressed(move |_gesture, _presses, x, y| {
            let (offset_x, offset_y, draw_width, draw_height) = fitted_image_rect(
                draw_area.width() as f64,
                draw_area.height() as f64,
                *selected_image_size.borrow(),
            );
            if draw_width <= 0.0
                || draw_height <= 0.0
                || x < offset_x
                || y < offset_y
                || x > offset_x + draw_width
                || y > offset_y + draw_height
            {
                return;
            }

            let point = VisionPointPromptDbus {
                x: ((x - offset_x) / draw_width).clamp(0.0, 1.0),
                y: ((y - offset_y) / draw_height).clamp(0.0, 1.0),
                positive: true,
            };
            match *prompt_edit_mode.borrow() {
                PromptEditMode::Add => segment_prompts.borrow_mut().push(point),
                PromptEditMode::Remove => {
                    let mut prompts = segment_prompts.borrow_mut();
                    if let Some((index, _)) = prompts.iter().enumerate().min_by(|(_, a), (_, b)| {
                        prompt_distance_squared(&point, a)
                            .total_cmp(&prompt_distance_squared(&point, b))
                    }) {
                        prompts.remove(index);
                    }
                }
            }
            draw_area.queue_draw();
        });
        click_area.add_controller(click);
    }
    overlay_area.add_css_class("card");
    let image_frame = AspectFrame::builder()
        .ratio(aspect_ratio(16, 9))
        .obey_child(false)
        .hexpand(true)
        .build();
    image_frame.set_child(Some(&overlay_area));
    vbox.append(
        &Label::builder()
            .label("Image preview, prompt selection, and depth map")
            .xalign(0.0)
            .build(),
    );

    let depth_canvas = DrawingArea::builder()
        .hexpand(true)
        .vexpand(true)
        .width_request(360)
        .height_request(240)
        .build();
    {
        let depth_map = depth_map.clone();
        depth_canvas.set_draw_func(move |_area, cr, width, height| {
            let Some(depth) = depth_map.borrow().clone() else {
                cr.set_source_rgb(0.08, 0.08, 0.08);
                let _ = cr.paint();
                return;
            };
            let depth_width = depth.width.max(1) as usize;
            let depth_height = depth.height.max(1) as usize;
            let cell_width = width as f64 / depth_width as f64;
            let cell_height = height as f64 / depth_height as f64;
            for y in 0..depth_height {
                for x in 0..depth_width {
                    let value = depth
                        .values
                        .get(y * depth_width + x)
                        .copied()
                        .unwrap_or(0.0)
                        .clamp(0.0, 1.0);
                    cr.set_source_rgb(value, 0.25 + value * 0.5, 1.0 - value);
                    cr.rectangle(
                        x as f64 * cell_width,
                        y as f64 * cell_height,
                        cell_width.ceil(),
                        cell_height.ceil(),
                    );
                    let _ = cr.fill();
                }
            }
        });
    }
    let depth_frame = AspectFrame::builder()
        .ratio(aspect_ratio(16, 9))
        .obey_child(false)
        .hexpand(true)
        .build();
    depth_frame.set_child(Some(&depth_canvas));
    let preview_row = Box::new(Orientation::Horizontal, 12);
    preview_row.append(&image_frame);
    preview_row.append(&depth_frame);
    vbox.append(&preview_row);

    let instructions_buffer = TextBuffer::new(None);
    let instructions_view = TextView::builder()
        .buffer(&instructions_buffer)
        .editable(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(false)
        .build();
    vbox.append(
        &Label::builder()
            .label("Per-image instructions (optional)")
            .xalign(0.0)
            .build(),
    );
    vbox.append(
        &ScrolledWindow::builder()
            .child(&instructions_view)
            .min_content_height(80)
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
        .label("Choose an image, then describe it locally.")
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

    let detections_buffer = TextBuffer::new(None);
    let detections_view = TextView::builder()
        .buffer(&detections_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Detections").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&detections_view)
            .min_content_height(140)
            .vexpand(true)
            .build(),
    );

    let masks_buffer = TextBuffer::new(None);
    let masks_view = TextView::builder()
        .buffer(&masks_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Masks").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&masks_view)
            .min_content_height(120)
            .vexpand(true)
            .build(),
    );

    let depth_buffer = TextBuffer::new(None);
    let depth_view = TextView::builder()
        .buffer(&depth_buffer)
        .editable(false)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .hexpand(true)
        .vexpand(true)
        .build();
    vbox.append(&Label::builder().label("Depth map").xalign(0.0).build());
    vbox.append(
        &ScrolledWindow::builder()
            .child(&depth_view)
            .min_content_height(120)
            .vexpand(true)
            .build(),
    );

    {
        let selected_image = selected_image.clone();
        let selected_image_size = selected_image_size.clone();
        let selected_label = selected_label.clone();
        let selected_image_surface = selected_image_surface.clone();
        let overlay_area = overlay_area.clone();
        let image_frame = image_frame.clone();
        let depth_frame = depth_frame.clone();
        let depth_canvas = depth_canvas.clone();
        let detections_overlay = detections_overlay.clone();
        let masks_overlay = masks_overlay.clone();
        let mask_surfaces = mask_surfaces.clone();
        let depth_map = depth_map.clone();
        let segment_prompts = segment_prompts.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        choose_button.connect_clicked(move |_| {
            let dialog = FileDialog::builder().title("Choose image").build();
            let selected_image = selected_image.clone();
            let selected_image_size = selected_image_size.clone();
            let selected_label = selected_label.clone();
            let selected_image_surface = selected_image_surface.clone();
            let overlay_area = overlay_area.clone();
            let image_frame = image_frame.clone();
            let depth_frame = depth_frame.clone();
            let depth_canvas = depth_canvas.clone();
            let detections_overlay = detections_overlay.clone();
            let masks_overlay = masks_overlay.clone();
            let mask_surfaces = mask_surfaces.clone();
            let depth_map = depth_map.clone();
            let segment_prompts = segment_prompts.clone();
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
                            let Some((surface, width, height)) = image_surface_from_bytes(&bytes)
                            else {
                                *selected_image.borrow_mut() = None;
                                *selected_image_size.borrow_mut() = None;
                                *selected_image_surface.borrow_mut() = None;
                                detections_overlay.borrow_mut().clear();
                                masks_overlay.borrow_mut().clear();
                                mask_surfaces.borrow_mut().clear();
                                *depth_map.borrow_mut() = None;
                                segment_prompts.borrow_mut().clear();
                                overlay_area.queue_draw();
                                depth_canvas.queue_draw();
                                selected_label.set_text("No file selected. Choose an image.");
                                status_title.set_text("Could not preview image");
                                status_detail
                                    .set_text("Selected file could not be decoded as PNG or JPEG.");
                                return;
                            };
                            *selected_image_size.borrow_mut() = Some((width, height));
                            *selected_image_surface.borrow_mut() = Some(surface);
                            image_frame.set_ratio(aspect_ratio(width, height));
                            depth_frame.set_ratio(aspect_ratio(width, height));
                            *selected_image.borrow_mut() = Some(bytes);
                            detections_overlay.borrow_mut().clear();
                            masks_overlay.borrow_mut().clear();
                            mask_surfaces.borrow_mut().clear();
                            *depth_map.borrow_mut() = None;
                            segment_prompts.borrow_mut().clear();
                            overlay_area.queue_draw();
                            depth_canvas.queue_draw();
                            selected_label.set_text(&format!("Selected: {}", path.display()));
                            status_title.set_text("Image selected");
                            status_detail.set_text(
                                "Click the preview to choose a segmentation point, or use the center default.",
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
        let instructions_buffer = instructions_buffer.clone();
        let description_buffer = description_buffer.clone();
        let action_buttons_for_click = action_buttons.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        describe_button.connect_clicked(move |_| {
            let image = match image_bytes_from_selection(&selected_image) {
                Ok(bytes) => bytes,
                Err(message) => {
                    status_title.set_text("No image input");
                    status_detail.set_text(&message);
                    return;
                }
            };
            let instructions = buffer_text(&instructions_buffer);

            description_buffer.set_text("");
            set_action_buttons_sensitive(&action_buttons_for_click, false);
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.describe session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let description_buffer = description_buffer.clone();
            let action_buttons = action_buttons_for_click.clone();
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
                        Ok(VisionEvent::Detections(_)) => {}
                        Ok(VisionEvent::Masks(_)) => {}
                        Ok(VisionEvent::Depth(_)) => {}
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Description failed");
                            status_detail.set_text(&message);
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Description complete");
                            status_detail
                                .set_text("Vision returned a description through the portal.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Description interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = describe_image(&image, &instructions, tx) {
                    eprintln!("[aileron-demo] describe error: {e}");
                    let _ = error_tx.send(VisionEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    {
        let selected_image = selected_image.clone();
        let instructions_buffer = instructions_buffer.clone();
        let ocr_buffer = ocr_buffer.clone();
        let action_buttons_for_click = action_buttons.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        ocr_button.connect_clicked(move |_| {
            let image = match image_bytes_from_selection(&selected_image) {
                Ok(bytes) => bytes,
                Err(message) => {
                    status_title.set_text("No image input");
                    status_detail.set_text(&message);
                    return;
                }
            };
            let instructions = buffer_text(&instructions_buffer);

            ocr_buffer.set_text("");
            set_action_buttons_sensitive(&action_buttons_for_click, false);
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.ocr session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let ocr_buffer = ocr_buffer.clone();
            let action_buttons = action_buttons_for_click.clone();
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
                        Ok(VisionEvent::Detections(_)) => {}
                        Ok(VisionEvent::Masks(_)) => {}
                        Ok(VisionEvent::Depth(_)) => {}
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Text extraction failed");
                            status_detail.set_text(&message);
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Text extraction complete");
                            status_detail
                                .set_text("Vision returned extracted text through the portal.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Text extraction interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = ocr_image(&image, &instructions, tx) {
                    eprintln!("[aileron-demo] ocr error: {e}");
                    let _ = error_tx.send(VisionEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    {
        let selected_image = selected_image.clone();
        let instructions_buffer = instructions_buffer.clone();
        let detections_buffer = detections_buffer.clone();
        let detections_overlay = detections_overlay.clone();
        let overlay_area = overlay_area.clone();
        let action_buttons_for_click = action_buttons.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        detect_button.connect_clicked(move |_| {
            let image = match image_bytes_from_selection(&selected_image) {
                Ok(bytes) => bytes,
                Err(message) => {
                    status_title.set_text("No image input");
                    status_detail.set_text(&message);
                    return;
                }
            };
            let instructions = buffer_text(&instructions_buffer);

            detections_buffer.set_text("");
            set_action_buttons_sensitive(&action_buttons_for_click, false);
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.detect session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let detections_buffer = detections_buffer.clone();
            let detections_overlay = detections_overlay.clone();
            let overlay_area = overlay_area.clone();
            let action_buttons = action_buttons_for_click.clone();
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
                        Ok(VisionEvent::Detections(detections)) => {
                            detections_buffer.set_text(&format_detections(&detections));
                            *detections_overlay.borrow_mut() = detections;
                            overlay_area.queue_draw();
                        }
                        Ok(VisionEvent::Masks(_)) => {}
                        Ok(VisionEvent::Depth(_)) => {}
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Detection failed");
                            status_detail.set_text(&message);
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Detection complete");
                            status_detail.set_text("Vision returned normalized object boxes.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Detection interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = detect_image(&image, &instructions, tx) {
                    eprintln!("[aileron-demo] detect error: {e}");
                    let _ = error_tx.send(VisionEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    {
        let selected_image = selected_image.clone();
        let instructions_buffer = instructions_buffer.clone();
        let masks_buffer = masks_buffer.clone();
        let masks_overlay = masks_overlay.clone();
        let mask_surfaces = mask_surfaces.clone();
        let overlay_area = overlay_area.clone();
        let segment_prompts = segment_prompts.clone();
        let action_buttons_for_click = action_buttons.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        segment_button.connect_clicked(move |_| {
            let image = match image_bytes_from_selection(&selected_image) {
                Ok(bytes) => bytes,
                Err(message) => {
                    status_title.set_text("No image input");
                    status_detail.set_text(&message);
                    return;
                }
            };
            let instructions = buffer_text(&instructions_buffer);
            masks_buffer.set_text("");
            set_action_buttons_sensitive(&action_buttons_for_click, false);
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.segment session with selected points...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let masks_buffer = masks_buffer.clone();
            let masks_overlay = masks_overlay.clone();
            let mask_surfaces = mask_surfaces.clone();
            let overlay_area = overlay_area.clone();
            let action_buttons = action_buttons_for_click.clone();
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
                        Ok(VisionEvent::Masks(masks)) => {
                            masks_buffer.set_text(&format_masks(&masks));
                            *mask_surfaces.borrow_mut() =
                                masks.iter().map(mask_surface_from_base64).collect();
                            *masks_overlay.borrow_mut() = masks;
                            overlay_area.queue_draw();
                        }
                        Ok(VisionEvent::Description(_))
                        | Ok(VisionEvent::Ocr(_))
                        | Ok(VisionEvent::Detections(_))
                        | Ok(VisionEvent::Depth(_)) => {}
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Segmentation failed");
                            status_detail.set_text(&message);
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Segmentation complete");
                            status_detail.set_text("Vision returned prompted masks.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Segmentation interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            let points = {
                let prompts = segment_prompts.borrow();
                if prompts.is_empty() {
                    vec![VisionPointPromptDbus {
                        x: 0.5,
                        y: 0.5,
                        positive: true,
                    }]
                } else {
                    prompts.clone()
                }
            };
            std::thread::spawn(move || {
                if let Err(e) = segment_image(&image, &instructions, points, tx) {
                    eprintln!("[aileron-demo] segment error: {e}");
                    let _ = error_tx.send(VisionEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    {
        let selected_image = selected_image.clone();
        let instructions_buffer = instructions_buffer.clone();
        let depth_buffer = depth_buffer.clone();
        let depth_map = depth_map.clone();
        let depth_frame = depth_frame.clone();
        let depth_canvas = depth_canvas.clone();
        let action_buttons_for_click = action_buttons.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        depth_button.connect_clicked(move |_| {
            let image = match image_bytes_from_selection(&selected_image) {
                Ok(bytes) => bytes,
                Err(message) => {
                    status_title.set_text("No image input");
                    status_detail.set_text(&message);
                    return;
                }
            };
            let instructions = buffer_text(&instructions_buffer);
            depth_buffer.set_text("");
            set_action_buttons_sensitive(&action_buttons_for_click, false);
            status_spinner.start();
            status_title.set_text("Creating vision session");
            status_detail.set_text("Opening a vision.depth session through the portal...");

            let (tx, rx) = std::sync::mpsc::channel::<VisionEvent>();
            let depth_buffer = depth_buffer.clone();
            let depth_map = depth_map.clone();
            let depth_frame = depth_frame.clone();
            let depth_canvas = depth_canvas.clone();
            let action_buttons = action_buttons_for_click.clone();
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
                        Ok(VisionEvent::Depth(depth)) => {
                            depth_buffer.set_text(&format_depth(&depth));
                            depth_frame.set_ratio(aspect_ratio(depth.width, depth.height));
                            *depth_map.borrow_mut() = Some(depth);
                            depth_canvas.queue_draw();
                        }
                        Ok(VisionEvent::Description(_))
                        | Ok(VisionEvent::Ocr(_))
                        | Ok(VisionEvent::Detections(_))
                        | Ok(VisionEvent::Masks(_)) => {}
                        Ok(VisionEvent::Error(message)) => {
                            status_spinner.stop();
                            status_title.set_text("Depth failed");
                            status_detail.set_text(&message);
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Ok(VisionEvent::Done) => {
                            status_spinner.stop();
                            status_title.set_text("Depth complete");
                            status_detail.set_text("Vision returned a normalized depth map.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            status_spinner.stop();
                            status_title.set_text("Depth interrupted");
                            status_detail
                                .set_text("The vision response channel closed unexpectedly.");
                            set_action_buttons_sensitive(&action_buttons, true);
                            return glib::ControlFlow::Break;
                        }
                    }
                }
                glib::ControlFlow::Continue
            });

            let error_tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(e) = depth_image(&image, &instructions, tx) {
                    eprintln!("[aileron-demo] depth error: {e}");
                    let _ = error_tx.send(VisionEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    scrollable_page(&vbox)
}

fn buffer_text(buffer: &TextBuffer) -> String {
    let (start, end) = buffer.bounds();
    buffer.text(&start, &end, false).trim().to_string()
}

fn image_bytes_from_selection(
    selected_image: &Rc<RefCell<Option<Vec<u8>>>>,
) -> Result<Vec<u8>, String> {
    if let Some(bytes) = selected_image.borrow().clone() {
        if bytes.is_empty() {
            return Err("Selected image is empty.".to_string());
        }
        return Ok(bytes);
    }

    Err("Choose an image file first.".to_string())
}

fn prompt_distance_squared(a: &VisionPointPromptDbus, b: &VisionPointPromptDbus) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    dx * dx + dy * dy
}

fn image_surface_from_bytes(bytes: &[u8]) -> Option<(cairo::ImageSurface, i32, i32)> {
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok()?;
    let width = texture.width();
    let height = texture.height();
    if width <= 0 || height <= 0 {
        return None;
    }

    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().ok()?;
        texture.download(&mut data, stride);
    }
    surface.mark_dirty();
    Some((surface, width, height))
}

fn mask_surface_from_base64(mask: &VisionMaskDbus) -> Option<cairo::ImageSurface> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&mask.mask_base64)
        .ok()?;
    let image = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    surface_from_rgba_image(&image, 96)
}

fn surface_from_rgba_image(image: &image::RgbaImage, max_alpha: u8) -> Option<cairo::ImageSurface> {
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    let mut surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, width as i32, height as i32).ok()?;
    let stride = surface.stride() as usize;
    {
        let mut data = surface.data().ok()?;
        for y in 0..height as usize {
            for x in 0..width as usize {
                let [red, green, blue, alpha] = image.get_pixel(x as u32, y as u32).0;
                let coverage = if max_alpha == 255 || alpha < 255 {
                    alpha
                } else {
                    red.max(green).max(blue)
                };
                let overlay_alpha = ((coverage as u16 * max_alpha as u16) / 255) as u8;
                let offset = y * stride + x * 4;
                if max_alpha == 255 {
                    data[offset] = ((blue as u16 * overlay_alpha as u16) / 255) as u8;
                    data[offset + 1] = ((green as u16 * overlay_alpha as u16) / 255) as u8;
                    data[offset + 2] = ((red as u16 * overlay_alpha as u16) / 255) as u8;
                } else {
                    data[offset] = ((255u16 * overlay_alpha as u16) / 255) as u8;
                    data[offset + 1] = ((64u16 * overlay_alpha as u16) / 255) as u8;
                    data[offset + 2] = ((179u16 * overlay_alpha as u16) / 255) as u8;
                }
                data[offset + 3] = overlay_alpha;
            }
        }
    }
    surface.mark_dirty();
    Some(surface)
}

fn fitted_image_rect(
    area_width: f64,
    area_height: f64,
    image_size: Option<(i32, i32)>,
) -> (f64, f64, f64, f64) {
    let Some((image_width, image_height)) = image_size else {
        return (0.0, 0.0, area_width, area_height);
    };
    if image_width <= 0 || image_height <= 0 || area_width <= 0.0 || area_height <= 0.0 {
        return (0.0, 0.0, area_width, area_height);
    }

    let scale = (area_width / image_width as f64).min(area_height / image_height as f64);
    let draw_width = image_width as f64 * scale;
    let draw_height = image_height as f64 * scale;
    (
        (area_width - draw_width) / 2.0,
        (area_height - draw_height) / 2.0,
        draw_width,
        draw_height,
    )
}

fn detection_color(index: usize) -> (f64, f64, f64) {
    const COLORS: [(f64, f64, f64); 8] = [
        (0.10, 0.55, 1.00),
        (1.00, 0.35, 0.20),
        (0.20, 0.80, 0.35),
        (0.85, 0.35, 1.00),
        (1.00, 0.75, 0.10),
        (0.10, 0.85, 0.85),
        (1.00, 0.45, 0.75),
        (0.65, 0.85, 0.20),
    ];

    COLORS[index % COLORS.len()]
}

fn aspect_ratio(width: i32, height: i32) -> f32 {
    if width <= 0 || height <= 0 {
        return 16.0 / 9.0;
    }

    width as f32 / height as f32
}

fn set_action_buttons_sensitive(buttons: &Rc<Vec<Button>>, sensitive: bool) {
    for button in buttons.iter() {
        button.set_sensitive(sensitive);
    }
}
