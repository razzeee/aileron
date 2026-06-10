/// Models page — list pulled OCI images, pull new images, assign use-cases, delete.
use gtk4::prelude::*;
use gtk4::{Box, Button, CheckButton, Entry, Label, ListBox, Orientation, ProgressBar,
           ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, AlertDialog, PreferencesGroup, PreferencesPage};

const USE_CASES: &[&str] = &[
    "llm.summarize",
    "llm.translate",
    "llm.rephrase",
    "llm.classify",
    "llm.extract",
    "asr.transcribe",
    "vision.describe",
    "vision.segment",
];

pub fn build() -> gtk4::Widget {
    let page = PreferencesPage::new();

    // ── Pull group ────────────────────────────────────────────────────────────
    let pull_group = PreferencesGroup::new();
    pull_group.set_title("Pull Model");
    pull_group.set_description(Some("Enter an OCI image reference to pull a new model."));

    let image_entry = Entry::builder()
        .placeholder_text("ghcr.io/aileron/llama3.2-3b-instruct:latest")
        .hexpand(true)
        .build();

    let pull_button = Button::with_label("Pull");
    pull_button.add_css_class("suggested-action");

    let progress = ProgressBar::new();
    progress.set_visible(false);

    let pull_row_box = Box::new(Orientation::Horizontal, 8);
    pull_row_box.set_margin_top(8);
    pull_row_box.set_margin_bottom(4);
    pull_row_box.append(&image_entry);
    pull_row_box.append(&pull_button);

    let pull_vbox = Box::new(Orientation::Vertical, 4);
    pull_vbox.set_margin_top(4);
    pull_vbox.set_margin_bottom(8);
    pull_vbox.set_margin_start(12);
    pull_vbox.set_margin_end(12);
    pull_vbox.append(&pull_row_box);
    pull_vbox.append(&progress);

    pull_group.add(&pull_vbox);

    page.add(&pull_group);

    // ── Installed models group ────────────────────────────────────────────────
    let models_group = PreferencesGroup::new();
    models_group.set_title("Installed Models");

    let refresh_button = Button::with_label("Refresh");
    models_group.set_header_suffix(Some(&refresh_button));

    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    // Wire up pull button now that list_box exists.
    {
        let entry = image_entry.clone();
        let progress = progress.clone();
        let list_box = list_box.clone();
        pull_button.connect_clicked(move |btn| {
            let image_ref = entry.text().to_string();
            if image_ref.is_empty() {
                return;
            }
            progress.set_visible(true);
            progress.pulse();

            let progress_clone = progress.clone();
            let list_box_clone = list_box.clone();
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());

            glib::spawn_future_local(async move {
                let result = gio::spawn_blocking(move || {
                    use aileron_varlink::aileron_Models::VarlinkClientInterface;
                    if let Ok(conn) = aileron_ipc::client::connect() {
                        let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                        let mut last_reply = None;
                        if let Ok(mut call) = client.pull(image_ref).more() {
                            for r in &mut call {
                                if let Ok(reply) = r {
                                    if reply.progress.done {
                                        last_reply = Some((reply.auto_assigned, reply.conflicts));
                                    }
                                }
                            }
                        }
                        last_reply
                    } else {
                        None
                    }
                })
                .await
                .ok()
                .flatten();

                progress_clone.set_fraction(1.0);
                progress_clone.set_visible(false);
                refresh_model_list(&list_box_clone);

                if let Some((auto_assigned, conflicts)) = result {
                    if !auto_assigned.is_empty() || !conflicts.is_empty() {
                        show_pull_result_dialog(window.as_ref(), auto_assigned, conflicts, list_box_clone);
                    }
                }
            });
        });
    }

    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(200)
        .child(&list_box)
        .build();
    models_group.add(&scroll);

    {
        let list_box = list_box.clone();
        refresh_button.connect_clicked(move |_| refresh_model_list(&list_box));
    }
    refresh_model_list(&list_box);

    page.add(&models_group);
    page.upcast()
}

fn refresh_model_list(list_box: &ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    use aileron_varlink::aileron_Models::VarlinkClientInterface;
    let conn = match aileron_ipc::client::connect() {
        Ok(c) => c,
        Err(e) => {
            let row = ActionRow::new();
            row.set_title(&format!("Error: {e}"));
            list_box.append(&row);
            return;
        }
    };

    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
    match client.list().call() {
        Ok(reply) => {
            if reply.models.is_empty() {
                let row = ActionRow::new();
                row.set_title("No models installed");
                list_box.append(&row);
                return;
            }
            for model in &reply.models {
                let row = ActionRow::new();
                row.set_title(&model.image_ref);
                let size_mb = model.size_bytes / 1_000_000;
                let use_cases = if model.use_cases.is_empty() {
                    "none".to_string()
                } else {
                    model.use_cases.join(", ")
                };
                row.set_subtitle(&format!("{size_mb} MB  ·  use-cases: {use_cases}"));

                // ── Assign button ─────────────────────────────────────────────
                let assign_btn = Button::with_label("Assign");
                assign_btn.set_valign(gtk4::Align::Center);
                let image_ref_assign = model.image_ref.clone();
                let current_use_cases = model.use_cases.clone();
                let list_box_assign = list_box.clone();
                assign_btn.connect_clicked(move |btn| {
                    let dialog = AlertDialog::builder()
                        .heading("Assign use-cases")
                        .body(&format!("Select use-cases for:\n{}", image_ref_assign))
                        .build();
                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("assign", "Assign");
                    dialog.set_response_appearance(
                        "assign",
                        libadwaita::ResponseAppearance::Suggested,
                    );
                    dialog.set_default_response(Some("assign"));
                    dialog.set_close_response("cancel");

                    // Build a checkbox for each use-case, pre-checked if already assigned.
                    let vbox = Box::new(Orientation::Vertical, 4);
                    vbox.set_margin_top(12);
                    let checkboxes: Vec<(CheckButton, &str)> = USE_CASES
                        .iter()
                        .map(|&uc| {
                            let cb = CheckButton::with_label(uc);
                            cb.set_active(current_use_cases.contains(&uc.to_string()));
                            vbox.append(&cb);
                            (cb, uc)
                        })
                        .collect();
                    dialog.set_extra_child(Some(&vbox));

                    let image_ref2 = image_ref_assign.clone();
                    let list_box2 = list_box_assign.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response != "assign" {
                            return;
                        }
                        let selected: Vec<String> = checkboxes
                            .iter()
                            .filter(|(cb, _)| cb.is_active())
                            .map(|(_, uc)| uc.to_string())
                            .collect();
                        let image_ref3 = image_ref2.clone();
                        let list_box3 = list_box2.clone();
                        glib::spawn_future_local(async move {
                            let _ = gio::spawn_blocking(move || {
                                use aileron_varlink::aileron_Models::VarlinkClientInterface;
                                if let Ok(conn) = aileron_ipc::client::connect() {
                                    let mut c =
                                        aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                                    for use_case in selected {
                                        let _ = c
                                            .assign_use_case(image_ref3.clone(), use_case)
                                            .call();
                                    }
                                }
                            })
                            .await;
                            refresh_model_list(&list_box3);
                        });
                    });

                    if let Some(window) = btn
                        .root()
                        .and_then(|r| r.downcast::<gtk4::Window>().ok())
                    {
                        dialog.present(Some(&window));
                    }
                });
                row.add_suffix(&assign_btn);

                // ── Delete button ─────────────────────────────────────────────
                let delete_btn = Button::with_label("Delete");
                delete_btn.add_css_class("destructive-action");
                delete_btn.set_valign(gtk4::Align::Center);
                let image_ref = model.image_ref.clone();
                let list_box_ref = list_box.clone();
                delete_btn.connect_clicked(move |_| {
                    use aileron_varlink::aileron_Models::VarlinkClientInterface;
                    if let Ok(conn) = aileron_ipc::client::connect() {
                        let mut c = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                        let _ = c.delete(image_ref.clone()).call();
                    }
                    refresh_model_list(&list_box_ref);
                });
                row.add_suffix(&delete_btn);
                list_box.append(&row);
            }
        }
        Err(e) => {
            let row = ActionRow::new();
            row.set_title(&format!("Error listing models: {e}"));
            list_box.append(&row);
        }
    }
}

/// Show auto-assignment feedback and present a conflict resolution dialog.
fn show_pull_result_dialog(
    window: Option<&gtk4::Window>,
    auto_assigned: Vec<String>,
    conflicts: Vec<aileron_varlink::aileron_Models::UseCaseConflict>,
    list_box: ListBox,
) {
    if conflicts.is_empty() {
        // Just a toast-style info dialog for auto-assignments.
        let msg = format!("Auto-assigned: {}", auto_assigned.join(", "));
        let dialog = AlertDialog::builder()
            .heading("Model assigned")
            .body(&msg)
            .build();
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.present(window);
        return;
    }

    // Build conflict resolution dialog — one row per conflict.
    let dialog = AlertDialog::builder()
        .heading("Use-case conflicts")
        .body("These use-cases are already assigned. Reassign to the new model?")
        .build();
    dialog.add_response("cancel", "Keep existing");
    dialog.add_response("reassign", "Reassign all");
    dialog.set_response_appearance("reassign", libadwaita::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("reassign"));
    dialog.set_close_response("cancel");

    let vbox = Box::new(Orientation::Vertical, 6);
    vbox.set_margin_top(12);
    for c in &conflicts {
        let row_label = Label::builder()
            .label(&format!(
                "<b>{}</b>\n<small>{} → {}</small>",
                c.use_case, c.current_image, c.new_image
            ))
            .use_markup(true)
            .xalign(0.0)
            .build();
        vbox.append(&row_label);
    }
    dialog.set_extra_child(Some(&vbox));

    dialog.connect_response(None, move |_, response| {
        if response != "reassign" {
            return;
        }
        let conflicts_clone = conflicts.clone();
        let list_box_clone = list_box.clone();
        glib::spawn_future_local(async move {
            let _ = gio::spawn_blocking(move || {
                use aileron_varlink::aileron_Models::VarlinkClientInterface;
                if let Ok(conn) = aileron_ipc::client::connect() {
                    let mut c = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                    for conflict in &conflicts_clone {
                        let _ = c
                            .assign_use_case(conflict.new_image.clone(), conflict.use_case.clone())
                            .call();
                    }
                }
            })
            .await;
            refresh_model_list(&list_box_clone);
        });
    });

    dialog.present(window);
}
