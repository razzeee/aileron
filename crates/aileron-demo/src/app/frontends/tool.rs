use super::super::friendly_error;
use super::super::tool_demo::{ToolDemoCase, ToolEvent, run_tool_demo};
use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{
    Align, Box, Button, DropDown, Entry, Label, Orientation, ScrolledWindow, Spinner, TextBuffer,
    TextView,
};
use libadwaita::AlertDialog;
use libadwaita::prelude::*;

pub(crate) fn build_page() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    vbox.append(
        &Label::builder()
            .label("A tiny app-owned agent loop: the local model may request a tool, but the app asks for approval before it validates and executes it.")
            .xalign(0.0)
            .wrap(true)
            .build(),
    );

    let case_dropdown = DropDown::from_strings(&ToolDemoCase::labels());
    case_dropdown.set_selected(ToolDemoCase::CharacterCounter.index());
    vbox.append(&Label::builder().label("Demo case").xalign(0.0).build());
    vbox.append(&case_dropdown);

    let prompt_entry = Entry::builder()
        .text(ToolDemoCase::CharacterCounter.default_prompt())
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

    {
        let prompt_entry = prompt_entry.clone();
        case_dropdown.connect_selected_notify(move |dropdown| {
            let case = ToolDemoCase::from_index(dropdown.selected())
                .unwrap_or(ToolDemoCase::CharacterCounter);
            prompt_entry.set_text(case.default_prompt());
        });
    }

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
        .label(ToolDemoCase::CharacterCounter.ready_detail())
        .xalign(0.0)
        .wrap(true)
        .build();
    status_text.append(&status_title);
    status_text.append(&status_detail);
    status_row.append(&status_text);
    vbox.append(&status_row);

    {
        let status_detail = status_detail.clone();
        case_dropdown.connect_selected_notify(move |dropdown| {
            let case = ToolDemoCase::from_index(dropdown.selected())
                .unwrap_or(ToolDemoCase::CharacterCounter);
            status_detail.set_text(case.ready_detail());
        });
    }

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
        let case_dropdown = case_dropdown.clone();
        let run_button_for_click = run_button.clone();
        let run_button = run_button.clone();
        let status_spinner = status_spinner.clone();
        let status_title = status_title.clone();
        let status_detail = status_detail.clone();
        let trace_buffer = trace_buffer.clone();
        run_button_for_click.connect_clicked(move |_| {
            let case = ToolDemoCase::from_index(case_dropdown.selected())
                .unwrap_or(ToolDemoCase::CharacterCounter);
            let prompt = prompt_entry.text().trim().to_string();
            if prompt.is_empty() {
                return;
            }

            trace_buffer.set_text("");
            run_button.set_sensitive(false);
            status_spinner.start();
            status_title.set_text("Running tool loop");
            status_detail.set_text(case.running_detail());

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
                        Ok(ToolEvent::ConfirmationRequested {
                            tool_name,
                            arguments_json,
                            response_tx,
                        }) => {
                            status_title_for_rx.set_text("Approve tool call");
                            status_detail_for_rx.set_text(
                                "The portal returned a model-requested tool call. The demo app will only execute it if you approve.",
                            );
                            let dialog = AlertDialog::builder()
                                .heading("Approve app-owned tool call?")
                                .body(format_tool_confirmation_body(
                                    case,
                                    &tool_name,
                                    &arguments_json,
                                ))
                                .build();
                            dialog.add_response("cancel", "Cancel");
                            dialog.add_response("run", "Run Tool");
                            dialog.set_close_response("cancel");
                            let parent = run_button_for_rx
                                .root()
                                .and_then(|root| root.downcast::<gtk4::Window>().ok());
                            dialog.connect_response(None, move |_, response| {
                                let _ = response_tx.send(response == "run");
                            });
                            dialog.present(parent.as_ref());
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
                        Ok(ToolEvent::Cancelled(message)) => {
                            status_spinner_for_rx.stop();
                            status_title_for_rx.set_text("Tool execution cancelled");
                            status_detail_for_rx.set_text(&message);
                            run_button_for_rx.set_sensitive(true);
                            return glib::ControlFlow::Break;
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
                if let Err(e) = run_tool_demo(case, &prompt, tx) {
                    eprintln!("[aileron-demo] tool demo error: {e}");
                    let _ = error_tx.send(ToolEvent::Error(friendly_error(&e)));
                }
            });
        });
    }

    scrollable_page(&vbox)
}

fn format_tool_confirmation_body(
    case: ToolDemoCase,
    tool_name: &str,
    arguments_json: &str,
) -> String {
    format!(
        "The Aileron portal does not run tools. It returned this request to the demo app, and the app is asking before it validates and executes anything.\n\n{}\n\nTool: {tool_name}\n\nArguments:\n{}",
        tool_safety_context(case),
        truncate_confirmation_text(arguments_json, 4_000)
    )
}

fn tool_safety_context(case: ToolDemoCase) -> &'static str {
    match case {
        ToolDemoCase::CharacterCounter => {
            "Safety context: this demo tool is deterministic and only counts characters in the supplied text."
        }
        ToolDemoCase::LinuxDiagnostics => {
            "Safety context: this demo only runs bounded, read-only Linux diagnostic commands. It does not apply fixes or change system state."
        }
    }
}

fn truncate_confirmation_text(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= max_chars {
            out.push_str("\n...[truncated]");
            return out;
        }
        out.push(ch);
    }
    out
}
