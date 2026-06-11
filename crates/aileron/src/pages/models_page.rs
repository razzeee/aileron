/// Profiles page — list installed profiles, add profiles, assign use-cases, delete.
use gtk4::prelude::*;
use gtk4::{
    Box, Button, CheckButton, ComboBoxText, Entry, Grid, Label, ListBox, Orientation, ProgressBar,
    ScrolledWindow,
};
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

    let progress = ProgressBar::new();
    progress.set_visible(false);

    // ── Installed profiles group ──────────────────────────────────────────────
    let models_group = PreferencesGroup::new();
    models_group.set_title("Installed Profiles");

    let refresh_button = Button::with_label("Refresh");
    let import_button = Button::with_label("Add Profile...");
    import_button.add_css_class("suggested-action");

    let header = Box::new(Orientation::Horizontal, 8);
    header.append(&refresh_button);
    header.append(&import_button);
    models_group.set_header_suffix(Some(&header));

    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    {
        let list_box = list_box.clone();
        let progress = progress.clone();
        import_button.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            show_url_install_dialog(window.as_ref(), list_box.clone(), progress.clone());
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

#[derive(Clone)]
struct UrlInstallRequest {
    runtime_id: String,
    url: String,
    sha256: String,
    use_cases: Vec<String>,
}

impl UrlInstallRequest {
    fn is_valid(&self) -> bool {
        !self.runtime_id.is_empty()
            && !self.url.is_empty()
            && !self.sha256.is_empty()
            && !self.use_cases.is_empty()
    }
}

#[derive(Clone)]
struct ProfileDetails {
    profile_id: String,
    model_id: String,
    runtime_id: String,
    artifact_path: String,
    runtime_images: Vec<String>,
    use_cases: Vec<String>,
    assigned_use_cases: Vec<String>,
}

fn form_entry(title: &str, placeholder: &str) -> (Box, Entry) {
    let row = Box::new(Orientation::Vertical, 4);
    let label = Label::new(Some(title));
    label.set_halign(gtk4::Align::Start);
    label.add_css_class("heading");
    let entry = Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .build();
    row.append(&label);
    row.append(&entry);
    (row, entry)
}

fn show_url_install_dialog(
    window: Option<&gtk4::Window>,
    list_box: ListBox,
    progress: ProgressBar,
) {
    let runtimes = available_runtime_ids();
    let runtime_row = Box::new(Orientation::Vertical, 4);
    let runtime_label = Label::new(Some("Runtime"));
    runtime_label.set_halign(gtk4::Align::Start);
    runtime_label.add_css_class("heading");
    let runtime_id = ComboBoxText::new();
    for runtime in &runtimes {
        runtime_id.append_text(runtime);
    }
    if !runtimes.is_empty() {
        runtime_id.set_active(Some(0));
    }
    runtime_row.append(&runtime_label);
    runtime_row.append(&runtime_id);

    let (url_row, url) = form_entry("Model file URL", "https://example.com/path/model.gguf");
    let (sha_row, sha256) = form_entry("SHA-256", "...");

    let use_case_grid = Grid::builder().column_spacing(18).row_spacing(8).build();
    let use_case_checks: Vec<(CheckButton, &str)> = USE_CASES
        .iter()
        .enumerate()
        .map(|(index, &use_case)| {
            let check = CheckButton::with_label(use_case);
            use_case_grid.attach(&check, (index % 2) as i32, (index / 2) as i32, 1, 1);
            (check, use_case)
        })
        .collect();

    let runtime_hint = Label::new(Some(if runtimes.is_empty() {
        "No runtimes found. Install a runtime manifest first."
    } else {
        "Runtime choices come from manifests and installed profiles."
    }));
    runtime_hint.set_wrap(true);
    runtime_hint.set_xalign(0.0);
    runtime_hint.add_css_class("dim-label");

    let fields = Box::new(Orientation::Vertical, 12);
    fields.set_margin_top(12);
    fields.set_margin_bottom(6);
    fields.set_margin_start(6);
    fields.set_margin_end(6);
    fields.append(&runtime_row);
    fields.append(&runtime_hint);
    fields.append(&url_row);
    fields.append(&sha_row);
    let use_case_label = Label::new(Some("Use-cases"));
    use_case_label.set_halign(gtk4::Align::Start);
    use_case_label.add_css_class("heading");
    fields.append(&use_case_label);
    fields.append(&use_case_grid);

    let dialog = AlertDialog::builder()
        .heading("Add Profile")
        .body("Create an installed profile from a model file URL and runtime.")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("install", "Install");
    dialog.set_response_appearance("install", libadwaita::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("install"));
    dialog.set_close_response("cancel");
    dialog.set_extra_child(Some(&fields));

    let window_owned = window.cloned();
    dialog.connect_response(None, move |_, response| {
        if response != "install" {
            return;
        }
        let request = UrlInstallRequest {
            runtime_id: runtime_id
                .active_text()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            url: url.text().trim().to_string(),
            sha256: sha256.text().trim().to_string(),
            use_cases: selected_use_cases(&use_case_checks),
        };
        if !request.is_valid() {
            show_message(
                window_owned.as_ref(),
                "Install needs all fields",
                "Runtime, model file URL, SHA-256, and at least one use-case are required.",
            );
            return;
        }
        install_url_profile(
            request,
            list_box.clone(),
            progress.clone(),
            window_owned.clone(),
        );
    });

    dialog.present(window);
}

fn show_profile_details(window: Option<&gtk4::Window>, details: &ProfileDetails) {
    let list = ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("boxed-list");

    add_detail_row(&list, "Model", &details.model_id);
    add_detail_row(&list, "Runtime", &details.runtime_id);
    add_detail_row(&list, "Artifact Directory", &details.artifact_path);
    add_detail_row(&list, "Runtime Images", &details.runtime_images.join("\n"));
    add_detail_row(
        &list,
        "Supported Use-Cases",
        &join_or_none(&details.use_cases),
    );
    add_detail_row(
        &list,
        "Assigned Use-Cases",
        &join_or_none(&details.assigned_use_cases),
    );

    let scrolled = ScrolledWindow::builder()
        .min_content_width(520)
        .min_content_height(260)
        .max_content_height(420)
        .child(&list)
        .build();

    let dialog = AlertDialog::builder()
        .heading(&details.profile_id)
        .body("Profile metadata and runtime wiring.")
        .build();
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.set_extra_child(Some(&scrolled));
    dialog.present(window);
}

fn add_detail_row(list: &ListBox, title: &str, value: &str) {
    let row = ActionRow::new();
    row.set_title(title);
    row.set_subtitle(value);
    list.append(&row);
}

fn model_kind(runtime_id: &str) -> &'static str {
    if runtime_id.starts_with("llm-") {
        "Text"
    } else if runtime_id.starts_with("asr-") {
        "Speech"
    } else if runtime_id.starts_with("vision-") {
        "Vision"
    } else {
        "Runtime"
    }
}

fn assignment_count(use_cases: &[String]) -> String {
    match use_cases.len() {
        0 => "Unassigned".to_string(),
        1 => "1 use-case".to_string(),
        n => format!("{n} use-cases"),
    }
}

fn join_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(", ")
    }
}

fn selected_use_cases(checks: &[(CheckButton, &str)]) -> Vec<String> {
    checks
        .iter()
        .filter(|(check, _)| check.is_active())
        .map(|(_, use_case)| (*use_case).to_string())
        .collect()
}

fn profile_availability(use_cases: &[String]) -> String {
    if use_cases.is_empty() {
        return "Unassigned".to_string();
    }

    use aileron_varlink::aileron_Inference::VarlinkClientInterface;
    let Ok(conn) = aileron_ipc::client::connect() else {
        return "Unavailable: daemon not reachable".to_string();
    };
    let mut client = aileron_varlink::aileron_Inference::VarlinkClient::new(conn);
    match client
        .get_use_case_availability("org.aileron.Manager".to_string(), use_cases[0].clone())
        .call()
    {
        Ok(reply) if reply.availability.is_available => "Available".to_string(),
        Ok(reply) => format!("Unavailable: {}", reply.availability.reason),
        Err(e) => format!("Unavailable: {e}"),
    }
}

fn show_message(window: Option<&gtk4::Window>, heading: &str, body: &str) {
    let dialog = AlertDialog::builder().heading(heading).body(body).build();
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.present(window);
}

fn available_runtime_ids() -> Vec<String> {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;
    let Ok(conn) = aileron_ipc::client::connect() else {
        return Vec::new();
    };
    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
    let mut runtimes = Vec::new();
    if let Ok(reply) = client.list_runtime_manifests().call() {
        runtimes.extend(reply.runtimes.into_iter().map(|runtime| runtime.runtime_id));
    }
    if let Ok(reply) = client.list().call() {
        runtimes.extend(reply.profiles.into_iter().map(|profile| profile.runtime_id));
    }
    runtimes.sort();
    runtimes.dedup();
    runtimes
}

fn install_url_profile(
    request: UrlInstallRequest,
    list_box: ListBox,
    progress: ProgressBar,
    window: Option<gtk4::Window>,
) {
    progress.set_visible(true);
    progress.pulse();

    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            use aileron_varlink::aileron_Models::VarlinkClientInterface;

            let conn = aileron_ipc::client::connect().map_err(|e| e.to_string())?;
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            let mut last_reply = None;
            let mut install_call = client.install_url_profile(
                request.runtime_id,
                request.url,
                request.sha256,
                request.use_cases,
            );
            let mut call = install_call.more().map_err(|e| e.to_string())?;
            for reply in &mut call {
                let reply = reply.map_err(|e| e.to_string())?;
                if reply.progress.done {
                    last_reply = Some((reply.auto_assigned, reply.conflicts));
                }
            }
            Ok::<_, String>(last_reply.unwrap_or_default())
        })
        .await;

        progress.set_fraction(1.0);
        progress.set_visible(false);
        refresh_model_list(&list_box);

        match result {
            Ok(Ok((auto_assigned, conflicts))) => {
                if !auto_assigned.is_empty() || !conflicts.is_empty() {
                    show_pull_result_dialog(window.as_ref(), auto_assigned, conflicts, list_box);
                }
            }
            Ok(Err(reason)) => show_message(window.as_ref(), "Install failed", &reason),
            Err(_) => show_message(window.as_ref(), "Install failed", "Install task failed"),
        }
    });
}

fn delete_profile(
    profile_id: String,
    force: bool,
    list_box: ListBox,
    window: Option<gtk4::Window>,
) {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;

    let result = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut c = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            c.delete_profile(profile_id.clone(), force)
                .call()
                .map_err(|e| e.to_string())
        });

    match result {
        Ok(_) => refresh_model_list(&list_box),
        Err(reason) if !force && reason.contains("ProfileInUse") => {
            let dialog = AlertDialog::builder()
                .heading("Profile is in use")
                .body("This profile is assigned or has active sessions. Delete it anyway?")
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("delete", "Delete Anyway");
            dialog.set_response_appearance("delete", libadwaita::ResponseAppearance::Destructive);
            dialog.set_close_response("cancel");

            let window_for_response = window.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "delete" {
                    delete_profile(
                        profile_id.clone(),
                        true,
                        list_box.clone(),
                        window_for_response.clone(),
                    );
                }
            });
            dialog.present(window.as_ref());
        }
        Err(reason) => show_message(window.as_ref(), "Delete failed", &reason),
    }
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
            if reply.profiles.is_empty() {
                let row = ActionRow::new();
                row.set_title("No profiles installed");
                row.set_subtitle("Add a profile from a model file URL to get started.");
                list_box.append(&row);
                return;
            }
            for model in &reply.profiles {
                let row = ActionRow::new();
                row.set_title(&model.profile_id);
                let availability = profile_availability(&model.assigned_use_cases);
                row.set_subtitle(&format!(
                    "{} · {} · {}",
                    availability,
                    model_kind(&model.runtime_id),
                    assignment_count(&model.assigned_use_cases)
                ));

                let details_btn = Button::with_label("Details");
                details_btn.set_valign(gtk4::Align::Center);
                let details = ProfileDetails {
                    profile_id: model.profile_id.clone(),
                    model_id: model.model_id.clone(),
                    runtime_id: model.runtime_id.clone(),
                    artifact_path: model.artifact_path.clone(),
                    runtime_images: model
                        .runtime_images
                        .iter()
                        .map(|image| format!("{}: {}", image.variant, image.image_ref))
                        .collect(),
                    use_cases: model.use_cases.clone(),
                    assigned_use_cases: model.assigned_use_cases.clone(),
                };
                details_btn.connect_clicked(move |btn| {
                    let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
                    show_profile_details(window.as_ref(), &details);
                });
                row.add_suffix(&details_btn);

                // ── Assign button ─────────────────────────────────────────────
                let assign_btn = Button::with_label("Assign");
                assign_btn.set_valign(gtk4::Align::Center);
                let profile_id_assign = model.profile_id.clone();
                let current_use_cases = model.assigned_use_cases.clone();
                let list_box_assign = list_box.clone();
                assign_btn.connect_clicked(move |btn| {
                    let dialog = AlertDialog::builder()
                        .heading("Assign use-cases")
                        .body(&format!("Select use-cases for:\n{}", profile_id_assign))
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

                    let profile_id2 = profile_id_assign.clone();
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
                        let profile_id3 = profile_id2.clone();
                        let list_box3 = list_box2.clone();
                        glib::spawn_future_local(async move {
                            let _ = gio::spawn_blocking(move || {
                                use aileron_varlink::aileron_Models::VarlinkClientInterface;
                                if let Ok(conn) = aileron_ipc::client::connect() {
                                    let mut c =
                                        aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                                    for use_case in selected {
                                        let _ =
                                            c.assign_use_case(profile_id3.clone(), use_case).call();
                                    }
                                }
                            })
                            .await;
                            refresh_model_list(&list_box3);
                        });
                    });

                    if let Some(window) = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok())
                    {
                        dialog.present(Some(&window));
                    }
                });
                row.add_suffix(&assign_btn);

                // ── Delete button ─────────────────────────────────────────────
                let delete_btn = Button::with_label("Delete");
                delete_btn.add_css_class("destructive-action");
                delete_btn.set_valign(gtk4::Align::Center);
                let profile_id = model.profile_id.clone();
                let list_box_ref = list_box.clone();
                delete_btn.connect_clicked(move |btn| {
                    let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
                    delete_profile(profile_id.clone(), false, list_box_ref.clone(), window);
                });
                row.add_suffix(&delete_btn);
                list_box.append(&row);
            }
        }
        Err(e) => {
            let row = ActionRow::new();
            row.set_title("Error listing profiles");
            row.set_subtitle(&format!(
                "{e}. If aileron-daemon is already running, restart it so its Varlink API matches this UI."
            ));
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
                c.use_case, c.current_profile, c.new_profile
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
                            .assign_use_case(
                                conflict.new_profile.clone(),
                                conflict.use_case.clone(),
                            )
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
