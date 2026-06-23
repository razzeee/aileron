/// Profiles page — list installed profiles, add profiles, assign use-cases, delete.
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use aileron_varlink::aileron_Models::InstallStatus;
use gtk4::prelude::*;
use gtk4::{Box, Button, CheckButton, Label, ListBox, Orientation, ProgressBar, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{
    ActionRow, AlertDialog, EntryRow, PreferencesGroup, ViewStack, ViewSwitcher, ViewSwitcherPolicy,
};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use super::{format_duration, format_speed, install_is_terminal_status, source_label};

const USE_CASES: &[&str] = &[
    "language.summarize",
    "language.translate",
    "language.rephrase",
    "language.complete",
    "language.classify",
    "language.extract",
    "language.analyze",
    "language.embed",
    "speech.transcribe",
    "speech.translate",
    "vision.describe",
    "vision.segment",
    "vision.ocr",
];

pub struct ModelsPage;

#[derive(Debug)]
pub enum ModelsMsg {}

pub struct ModelsWidgets;

impl SimpleComponent for ModelsPage {
    type Init = Rc<dyn Fn()>;
    type Input = ModelsMsg;
    type Output = ();
    type Widgets = ModelsWidgets;
    type Root = Box;

    fn init_root() -> Self::Root {
        Box::new(Orientation::Vertical, 0)
    }

    fn init(
        runtime_images_changed: Self::Init,
        root: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        root.append(&build_widget(runtime_images_changed));
        ComponentParts {
            model: ModelsPage,
            widgets: ModelsWidgets,
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {}
    }
}

fn build_widget(runtime_images_changed: Rc<dyn Fn()>) -> gtk4::Widget {
    let root = Box::new(Orientation::Vertical, 12);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let stack = ViewStack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    let switcher = ViewSwitcher::builder()
        .stack(&stack)
        .policy(ViewSwitcherPolicy::Wide)
        .halign(gtk4::Align::Center)
        .build();
    root.append(&switcher);
    root.append(&stack);

    let tasks_page = Box::new(Orientation::Vertical, 12);
    tasks_page.set_hexpand(true);
    tasks_page.set_vexpand(true);
    set_tab_content_margins(&tasks_page);
    let library_page = Box::new(Orientation::Vertical, 12);
    library_page.set_hexpand(true);
    library_page.set_vexpand(true);
    set_tab_content_margins(&library_page);
    let installed_page = Box::new(Orientation::Vertical, 12);
    installed_page.set_hexpand(true);
    installed_page.set_vexpand(true);
    set_tab_content_margins(&installed_page);

    // ── Installed profiles group ──────────────────────────────────────────────
    let models_group = PreferencesGroup::new();
    models_group.set_title("Installed profiles");

    let import_button = Button::with_label("Add profile...");
    import_button.add_css_class("suggested-action");

    let header = Box::new(Orientation::Horizontal, 8);
    header.append(&import_button);
    models_group.set_header_suffix(Some(&header));

    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let readiness_group = PreferencesGroup::new();
    readiness_group.set_title("Task readiness");
    readiness_group.set_description(Some(
        "Install and assign one profile for each task you want apps to use.",
    ));

    let readiness_box = ListBox::new();
    readiness_box.set_selection_mode(gtk4::SelectionMode::None);
    readiness_box.add_css_class("boxed-list");

    let library_group = PreferencesGroup::new();
    library_group.set_title("Profile library");
    library_group.set_description(Some(
        "Install manifest-backed profiles. Nothing is downloaded until you click Install.",
    ));

    let library_box = ListBox::new();
    library_box.set_selection_mode(gtk4::SelectionMode::None);
    library_box.add_css_class("boxed-list");

    let downloads_box = ListBox::new();
    downloads_box.set_selection_mode(gtk4::SelectionMode::None);
    downloads_box.add_css_class("boxed-list");

    let lists = ModelLists {
        profiles: list_box.clone(),
        readiness: readiness_box.clone(),
        library: library_box.clone(),
        downloads: downloads_box.clone(),
        install_poll_active: Rc::new(Cell::new(false)),
        runtime_images_changed,
    };

    {
        let lists = lists.clone();
        stack.connect_visible_child_name_notify(move |stack| {
            match stack.visible_child_name().as_deref() {
                Some("installed") => refresh_model_list(&lists),
                Some("profile-library") => refresh_library_list(&lists),
                Some("tasks") => refresh_readiness_list(&lists),
                _ => {}
            }
        });
    }

    {
        let lists = lists.clone();
        import_button.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            show_url_install_dialog(window.as_ref(), lists.clone());
        });
    }

    let readiness_scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&readiness_box)
        .build();
    readiness_group.add(&readiness_scroll);
    tasks_page.append(&readiness_group);

    let library_scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&library_box)
        .build();
    library_group.add(&library_scroll);
    library_page.append(&library_group);

    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&list_box)
        .build();
    models_group.add(&scroll);
    installed_page.append(&models_group);

    refresh_readiness_list(&lists);
    refresh_downloads_list(&lists);
    if has_active_installs() {
        start_install_poll(&lists);
    }

    let tasks_page = stack.add_titled(&tasks_page, Some("tasks"), "Tasks");
    tasks_page.set_icon_name(Some("checkbox-checked-symbolic"));
    let library_page = stack.add_titled(&library_page, Some("profile-library"), "Library");
    library_page.set_icon_name(Some("folder-symbolic"));
    let installed_page = stack.add_titled(&installed_page, Some("installed"), "Installed");
    installed_page.set_icon_name(Some("view-list-symbolic"));
    stack.set_visible_child_name("tasks");

    root.upcast()
}

#[derive(Clone)]
struct ModelLists {
    profiles: ListBox,
    readiness: ListBox,
    library: ListBox,
    downloads: ListBox,
    install_poll_active: Rc<Cell<bool>>,
    runtime_images_changed: Rc<dyn Fn()>,
}

fn set_tab_content_margins(page: &Box) {
    page.set_margin_top(12);
    page.set_margin_start(24);
    page.set_margin_end(24);
}

#[derive(Clone)]
struct UrlInstallRequest {
    runtime_id: String,
    url: String,
    sha256: String,
    mmproj_url: String,
    mmproj_sha256: String,
    use_cases: Vec<String>,
}

impl UrlInstallRequest {
    fn is_valid(&self) -> bool {
        !self.runtime_id.is_empty()
            && !self.url.is_empty()
            && !self.sha256.is_empty()
            && !self.use_cases.is_empty()
            && (self.mmproj_url.is_empty() == self.mmproj_sha256.is_empty())
    }
}

#[derive(Clone)]
struct ProfileDetails {
    profile_id: String,
    model_id: String,
    runtime_id: String,
    artifact_path: String,
    source: String,
    runtime_images: Vec<String>,
    use_cases: Vec<String>,
    assigned_use_cases: Vec<String>,
}

#[derive(Clone)]
struct CatalogProfileDetails {
    profile_id: String,
    model_id: String,
    spdx_license: String,
    runtime_id: String,
    tier: String,
    disk_size_gb: f64,
    min_ram_gb: f64,
    recommended_ram_gb: f64,
    min_vram_gb: f64,
    fit_score: f64,
    fit_level: String,
    recommended: bool,
    installing: bool,
    recommendation_reason: String,
    use_cases: Vec<String>,
}

fn form_entry(title: &str) -> EntryRow {
    EntryRow::builder().title(title).build()
}

fn show_url_install_dialog(window: Option<&gtk4::Window>, lists: ModelLists) {
    let runtimes = available_runtime_ids();
    let runtime_id = form_entry("Runtime");
    if let Some(runtime) = runtimes.first() {
        runtime_id.set_text(runtime);
    }

    let url = form_entry("Model file URL");
    let sha256 = form_entry("SHA-256");
    let mmproj_url = form_entry("Vision projector URL (optional)");
    let mmproj_sha256 = form_entry("Vision projector SHA-256 (optional)");

    let use_case_box = Box::new(Orientation::Vertical, 6);
    let use_case_checks: Vec<(CheckButton, &str)> = USE_CASES
        .iter()
        .map(|&use_case| {
            let check = CheckButton::with_label(use_case);
            check.set_halign(gtk4::Align::Start);
            use_case_box.append(&check);
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

    let fields = Box::new(Orientation::Vertical, 18);
    fields.set_margin_top(6);
    fields.set_margin_bottom(6);

    let form_group = PreferencesGroup::new();
    form_group.add(&runtime_id);
    form_group.add(&url);
    form_group.add(&sha256);
    form_group.add(&mmproj_url);
    form_group.add(&mmproj_sha256);
    fields.append(&form_group);
    fields.append(&runtime_hint);
    let use_case_label = Label::new(Some("Use-cases"));
    use_case_label.set_halign(gtk4::Align::Start);
    use_case_label.add_css_class("heading");
    fields.append(&use_case_label);
    fields.append(&use_case_box);

    let dialog = AlertDialog::builder()
        .heading("Add profile")
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
            runtime_id: runtime_id.text().trim().to_string(),
            url: url.text().trim().to_string(),
            sha256: sha256.text().trim().to_string(),
            mmproj_url: mmproj_url.text().trim().to_string(),
            mmproj_sha256: mmproj_sha256.text().trim().to_string(),
            use_cases: selected_use_cases(&use_case_checks),
        };
        if !request.is_valid() {
            show_message(
                window_owned.as_ref(),
                "Install needs all fields",
                "Runtime, model file URL, SHA-256, and at least one use-case are required. If you add a vision projector, provide both its URL and SHA-256.",
            );
            return;
        }
        install_url_profile(request, lists.clone(), window_owned.clone());
    });

    dialog.present(window);
}

fn refresh_library_list(lists: &ModelLists) {
    let library = &lists.library;
    while let Some(child) = library.first_child() {
        library.remove(&child);
    }
    let row = ActionRow::new();
    row.set_title("Loading profile library");
    row.set_subtitle("Scoring profiles against this machine.");
    library.append(&row);

    let lists = lists.clone();
    glib::spawn_future_local(async move {
        let profiles = gio::spawn_blocking(move || {
            use aileron_varlink::aileron_Models::VarlinkClientInterface;

            aileron_ipc::client::connect()
                .map_err(|e| e.to_string())
                .and_then(|conn| {
                    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                    client.list_catalog().call().map_err(|e| e.to_string())
                })
        })
        .await
        .map_err(|_| "Profile library task failed".to_string())
        .and_then(|result| result);

        render_library_list(&lists, profiles);
    });
}

fn render_library_list(
    lists: &ModelLists,
    profiles: Result<aileron_varlink::aileron_Models::ListCatalog_Reply, String>,
) {
    let library = &lists.library;
    while let Some(child) = library.first_child() {
        library.remove(&child);
    }

    let profiles = match profiles {
        Ok(reply) => reply.profiles,
        Err(reason) => {
            let row = ActionRow::new();
            row.set_title("Profile library unavailable");
            row.set_subtitle(&reason);
            library.append(&row);
            return;
        }
    };

    if profiles.is_empty() {
        let row = ActionRow::new();
        row.set_title("No profiles found");
        row.set_subtitle("Install model manifests under a manifests/models directory.");
        library.append(&row);
    }

    for profile in profiles {
        let row = ActionRow::new();
        row.set_title(&profile.profile_id);
        let recommendation_reason = profile.recommendation_reason.clone();
        let recommended = if profile.installing {
            "Installing"
        } else {
            fit_label(&profile.fit_level, profile.recommended)
        };
        let memory = if profile.recommended_ram_gb > profile.min_ram_gb {
            format!(
                "{:.1} GB min / {:.1} GB recommended RAM",
                profile.min_ram_gb, profile.recommended_ram_gb
            )
        } else {
            format!("{:.1} GB RAM", profile.min_ram_gb)
        };
        let fit = fit_score_label(profile.fit_score)
            .map(|score| format!(" · Fit {score}"))
            .unwrap_or_default();
        row.set_subtitle(&format!(
            "{}{} · {} · {} · {} · {}",
            recommended,
            fit,
            profile.tier,
            model_kind(&profile.runtime_id),
            format_size(profile.disk_size_gb),
            memory
        ));
        row.set_tooltip_text(Some(&recommendation_reason));

        let details = CatalogProfileDetails {
            profile_id: profile.profile_id.clone(),
            model_id: profile.model_id,
            spdx_license: profile.spdx_license.unwrap_or_default(),
            runtime_id: profile.runtime_id,
            tier: profile.tier,
            disk_size_gb: profile.disk_size_gb,
            min_ram_gb: profile.min_ram_gb,
            recommended_ram_gb: profile.recommended_ram_gb,
            min_vram_gb: profile.min_vram_gb,
            fit_score: profile.fit_score,
            fit_level: profile.fit_level,
            recommended: profile.recommended,
            installing: profile.installing,
            recommendation_reason,
            use_cases: profile.use_cases,
        };

        let install_btn = Button::with_label("Install");
        install_btn.set_valign(gtk4::Align::Center);
        install_btn.set_sensitive(!profile.installing);
        let profile_id = profile.profile_id.clone();
        let lists_for_install = lists.clone();
        install_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            install_catalog_profile(profile_id.clone(), lists_for_install.clone(), window);
        });
        row.add_suffix(&install_btn);

        let details_btn = Button::with_label("Details");
        details_btn.set_valign(gtk4::Align::Center);
        details_btn.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            show_catalog_profile_details(window.as_ref(), &details);
        });
        row.add_suffix(&details_btn);
        library.append(&row);
    }
}

fn show_catalog_profile_details(window: Option<&gtk4::Window>, details: &CatalogProfileDetails) {
    let list = ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("boxed-list");
    add_detail_row(&list, "Model", &details.model_id);
    if !details.spdx_license.is_empty() {
        add_detail_row(&list, "License", &details.spdx_license);
    }
    add_detail_row(&list, "Runtime", &details.runtime_id);
    add_detail_row(&list, "Tier", &details.tier);
    add_detail_row(&list, "Install size", &format_size(details.disk_size_gb));
    add_detail_row(
        &list,
        "Minimum RAM",
        &format!("{:.1} GB", details.min_ram_gb),
    );
    if details.recommended_ram_gb > 0.0 {
        add_detail_row(
            &list,
            "Recommended RAM",
            &format!("{:.1} GB", details.recommended_ram_gb),
        );
    }
    if details.min_vram_gb > 0.0 {
        add_detail_row(
            &list,
            "Published VRAM target",
            &format!("{:.1} GB", details.min_vram_gb),
        );
    }
    if details.fit_score > 0.0 {
        add_detail_row(&list, "Fit", &fit_score_label(details.fit_score).unwrap());
    }
    add_detail_row(
        &list,
        "Recommendation",
        if details.installing {
            "Installing"
        } else {
            fit_label(&details.fit_level, details.recommended)
        },
    );
    add_detail_row(&list, "Reason", &details.recommendation_reason);
    add_detail_row(
        &list,
        "Supported use-cases",
        &join_or_none(&details.use_cases),
    );

    let scrolled = ScrolledWindow::builder()
        .min_content_width(520)
        .min_content_height(260)
        .max_content_height(420)
        .child(&list)
        .build();
    let dialog = AlertDialog::builder()
        .heading(&details.profile_id)
        .body("Profile library metadata.")
        .build();
    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.set_extra_child(Some(&scrolled));
    dialog.present(window);
}

fn show_profile_details(window: Option<&gtk4::Window>, details: &ProfileDetails) {
    let list = ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("boxed-list");

    add_detail_row(&list, "Model", &details.model_id);
    add_detail_row(&list, "Runtime", &details.runtime_id);
    add_detail_row(&list, "Source", &details.source);
    add_detail_row(&list, "Artifact directory", &details.artifact_path);
    add_detail_row(&list, "Runtime images", &details.runtime_images.join("\n"));
    add_detail_row(
        &list,
        "Supported use-cases",
        &join_or_none(&details.use_cases),
    );
    add_detail_row(
        &list,
        "Assigned use-cases",
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

fn fit_label(fit_level: &str, recommended: bool) -> &'static str {
    match fit_level {
        "recommended" => "Recommended",
        "fits_minimum" => "Fits minimum",
        "too_large" => "Too large",
        "unknown" => "Unknown fit",
        _ if recommended => "Recommended",
        _ => "Optional",
    }
}

fn fit_score_label(score: f64) -> Option<String> {
    if score > 0.0 {
        Some(format!("{score:.0}/100"))
    } else {
        None
    }
}

fn format_size(gb: f64) -> String {
    if gb >= 0.1 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", gb * 1024.0)
    }
}

fn format_profile_size(bytes: i64) -> String {
    if bytes <= 0 {
        return "unknown size".to_string();
    }

    let bytes = bytes as f64;
    let kib = bytes / 1024.0;
    if kib < 1.0 {
        return format!("{} B", bytes as i64);
    }

    let mib = kib / 1024.0;
    if mib < 1.0 {
        return format!("{kib:.0} KB");
    }

    let gib = mib / 1024.0;
    if gib < 1.0 {
        return format!("{mib:.0} MB");
    }

    format!("{gib:.1} GB")
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
    lists: ModelLists,
    window: Option<gtk4::Window>,
) {
    start_install_poll(&lists);

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
                request.mmproj_url,
                request.mmproj_sha256,
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

        refresh_model_page(&lists);

        match result {
            Ok(Ok((auto_assigned, conflicts))) => {
                if !conflicts.is_empty() {
                    show_pull_result_dialog(window.as_ref(), auto_assigned, conflicts, lists);
                }
            }
            Ok(Err(reason)) if is_non_error_install_result(&reason) => {
                refresh_model_page(&lists);
            }
            Ok(Err(reason)) => show_message(
                window.as_ref(),
                "Install failed",
                &install_error_message(&reason),
            ),
            Err(_) => show_message(window.as_ref(), "Install failed", "Install task failed"),
        }
    });
}

fn install_catalog_profile(profile_id: String, lists: ModelLists, window: Option<gtk4::Window>) {
    start_install_poll(&lists);

    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            use aileron_varlink::aileron_Models::VarlinkClientInterface;

            let conn = aileron_ipc::client::connect().map_err(|e| e.to_string())?;
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            let mut last_reply = None;
            let mut install_call = client.install_manifest(profile_id);
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

        refresh_model_page(&lists);

        match result {
            Ok(Ok((auto_assigned, conflicts))) => {
                if !conflicts.is_empty() {
                    show_pull_result_dialog(window.as_ref(), auto_assigned, conflicts, lists);
                }
            }
            Ok(Err(reason)) if is_non_error_install_result(&reason) => {
                refresh_model_page(&lists);
            }
            Ok(Err(reason)) => show_message(
                window.as_ref(),
                "Install failed",
                &install_error_message(&reason),
            ),
            Err(_) => show_message(window.as_ref(), "Install failed", "Install task failed"),
        }
    });
}

fn is_non_error_install_result(reason: &str) -> bool {
    reason.contains("install already running") || reason.contains("install cancelled")
}

fn install_error_message(reason: &str) -> String {
    let reason = extract_varlink_reason(reason).unwrap_or(reason).trim();
    if let Some(image_ref) = reason.strip_prefix("local runtime image is not built: ") {
        return format!(
            "The required local runtime image is missing:\n\n{image_ref}\n\nBuild or tag this runtime image, then try the install again."
        );
    }
    reason.to_string()
}

fn extract_varlink_reason(reason: &str) -> Option<&str> {
    let marker = "reason: \"";
    let start = reason.find(marker)? + marker.len();
    let rest = &reason[start..];
    let end = rest.find('\"')?;
    Some(&rest[..end])
}

fn delete_profile(
    profile_id: String,
    force: bool,
    lists: ModelLists,
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
        Ok(_) => refresh_model_page(&lists),
        Err(reason) if !force && reason.contains("ProfileInUse") => {
            let dialog = AlertDialog::builder()
                .heading("Profile is in use")
                .body("This profile is assigned or has active sessions. Delete it anyway?")
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("delete", "Delete anyway");
            dialog.set_response_appearance("delete", libadwaita::ResponseAppearance::Destructive);
            dialog.set_close_response("cancel");

            let window_for_response = window.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "delete" {
                    delete_profile(
                        profile_id.clone(),
                        true,
                        lists.clone(),
                        window_for_response.clone(),
                    );
                }
            });
            dialog.present(window.as_ref());
        }
        Err(reason) => show_message(window.as_ref(), "Delete failed", &reason),
    }
}

fn refresh_model_page(lists: &ModelLists) {
    refresh_readiness_list(lists);
    refresh_library_list(lists);
    refresh_model_list(lists);
    refresh_downloads_list(lists);
    (lists.runtime_images_changed)();
}

fn start_install_poll(lists: &ModelLists) {
    if lists.install_poll_active.get() {
        return;
    }
    lists.install_poll_active.set(true);
    refresh_downloads_list(lists);
    let lists = lists.clone();
    let mut grace_ticks = 15;
    glib::timeout_add_seconds_local(2, move || {
        refresh_downloads_list(&lists);
        if has_active_installs() {
            grace_ticks = 15;
            glib::ControlFlow::Continue
        } else if grace_ticks > 0 {
            grace_ticks -= 1;
            glib::ControlFlow::Continue
        } else {
            lists.install_poll_active.set(false);
            glib::ControlFlow::Break
        }
    });
}

fn has_active_installs() -> bool {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;

    aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client.list_installs().call().map_err(|e| e.to_string())
        })
        .map(|reply| {
            reply
                .installs
                .iter()
                .any(|install| !install_is_terminal(install))
        })
        .unwrap_or(false)
}

fn refresh_readiness_list(lists: &ModelLists) {
    let list_box = &lists.readiness;
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
    let row = ActionRow::new();
    row.set_title("Loading tasks");
    row.set_subtitle("Checking installed and recommended profiles.");
    list_box.append(&row);

    let lists = lists.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            use aileron_varlink::aileron_Models::VarlinkClientInterface;

            aileron_ipc::client::connect()
                .map_err(|e| e.to_string())
                .and_then(|conn| {
                    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                    let profiles = client.list().call().map_err(|e| e.to_string())?.profiles;
                    let catalog = client
                        .list_catalog()
                        .call()
                        .map_err(|e| e.to_string())?
                        .profiles;
                    Ok::<_, String>((profiles, catalog))
                })
        })
        .await
        .map_err(|_| "Task readiness check failed".to_string())
        .and_then(|result| result);

        render_readiness_list(&lists, result);
    });
}

fn render_readiness_list(
    lists: &ModelLists,
    result: Result<
        (
            Vec<aileron_varlink::aileron_Models::ProfileInfo>,
            Vec<aileron_varlink::aileron_Models::CatalogProfileInfo>,
        ),
        String,
    >,
) {
    let list_box = &lists.readiness;
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    let (profiles, catalog) = match result {
        Ok(data) => data,
        Err(reason) => {
            let row = ActionRow::new();
            row.set_title("Cannot check use-case readiness");
            row.set_subtitle(&reason);
            list_box.append(&row);
            return;
        }
    };

    let installing: Vec<String> = catalog
        .iter()
        .filter(|profile| profile.installing)
        .map(|profile| profile.profile_id.clone())
        .collect();
    if !installing.is_empty() {
        start_install_poll(lists);
    }

    let assigned: HashSet<&str> = profiles
        .iter()
        .flat_map(|profile| profile.assigned_use_cases.iter().map(String::as_str))
        .collect();

    let mut rows = Vec::new();
    for &use_case in USE_CASES {
        if let Some(profile) = profiles
            .iter()
            .find(|profile| profile.assigned_use_cases.iter().any(|uc| uc == use_case))
        {
            let better = better_catalog_candidate(&catalog, &profile.profile_id, use_case);
            if let Some(candidate) = better {
                let detail = format!(
                    "Assigned to {} · {} · Better available: {} · {}",
                    profile.profile_id,
                    use_case_kind(use_case),
                    candidate.profile_id,
                    format_size(candidate.disk_size_gb)
                );
                let installed_better = profiles
                    .iter()
                    .any(|installed| installed.profile_id == candidate.profile_id);
                let button = if installed_better {
                    let button = Button::with_label("Assign better");
                    button.set_valign(gtk4::Align::Center);
                    let profile_id = candidate.profile_id.clone();
                    let use_case = use_case.to_string();
                    let lists_for_assign = lists.clone();
                    button.connect_clicked(move |btn| {
                        let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
                        assign_use_case_direct(
                            profile_id.clone(),
                            use_case.clone(),
                            lists_for_assign.clone(),
                            window,
                        );
                    });
                    Some(button)
                } else {
                    let button = Button::with_label("Install better");
                    button.set_valign(gtk4::Align::Center);
                    button.set_sensitive(!candidate.installing);
                    let profile_id = candidate.profile_id.clone();
                    let lists_for_install = lists.clone();
                    button.connect_clicked(move |btn| {
                        btn.set_sensitive(false);
                        let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
                        install_catalog_profile(
                            profile_id.clone(),
                            lists_for_install.clone(),
                            window,
                        );
                    });
                    Some(button)
                };
                let row = readiness_row(
                    use_case,
                    "Ready",
                    "success",
                    &detail,
                    button,
                    Some(&candidate.recommendation_reason),
                );
                rows.push((readiness_sort_key("Ready"), row));
            } else {
                let row = readiness_row(
                    use_case,
                    "Ready",
                    "success",
                    &format!(
                        "Assigned to {} · {}",
                        profile.profile_id,
                        use_case_kind(use_case)
                    ),
                    None,
                    None,
                );
                rows.push((readiness_sort_key("Ready"), row));
            }
            continue;
        }

        if let Some(profile) = profiles
            .iter()
            .find(|profile| profile.use_cases.iter().any(|uc| uc == use_case))
        {
            if let Some(candidate) =
                better_catalog_candidate(&catalog, &profile.profile_id, use_case)
            {
                let detail = format!(
                    "{} is installed but not assigned · Better available: {} · {}",
                    profile.profile_id,
                    candidate.profile_id,
                    format_size(candidate.disk_size_gb)
                );
                let button = recommendation_button(candidate, profiles.as_slice(), use_case, lists);
                let row = readiness_row(
                    use_case,
                    "Installed",
                    "accent",
                    &detail,
                    Some(button),
                    Some(&candidate.recommendation_reason),
                );
                rows.push((readiness_sort_key("Installed"), row));
            } else {
                let button = Button::with_label("Assign");
                button.set_valign(gtk4::Align::Center);
                let profile_id = profile.profile_id.clone();
                let assigned_use_case = use_case.to_string();
                let lists_for_assign = lists.clone();
                button.connect_clicked(move |btn| {
                    let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
                    assign_use_case_direct(
                        profile_id.clone(),
                        assigned_use_case.clone(),
                        lists_for_assign.clone(),
                        window,
                    );
                });
                let row = readiness_row(
                    use_case,
                    "Installed",
                    "accent",
                    &format!(
                        "{} is installed but not assigned · {}",
                        profile.profile_id,
                        use_case_kind(use_case)
                    ),
                    Some(button),
                    None,
                );
                rows.push((readiness_sort_key("Installed"), row));
            }
            continue;
        }

        let candidate = best_missing_candidate(&catalog, &assigned, use_case);

        if let Some(profile) = candidate {
            let status = if profile.installing {
                "Installing"
            } else {
                fit_label(&profile.fit_level, profile.recommended)
            };
            let detail = format!(
                "Recommended: {} · {} · {}",
                profile.profile_id,
                status,
                format_size(profile.disk_size_gb)
            );
            let button = Button::with_label("Install");
            button.set_valign(gtk4::Align::Center);
            button.set_sensitive(!profile.installing);
            let profile_id = profile.profile_id.clone();
            let lists_for_install = lists.clone();
            button.connect_clicked(move |btn| {
                btn.set_sensitive(false);
                let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
                install_catalog_profile(profile_id.clone(), lists_for_install.clone(), window);
            });
            let row = readiness_row(
                use_case,
                if profile.installing {
                    "Installing"
                } else {
                    "Missing"
                },
                "warning",
                &detail,
                Some(button),
                Some(&profile.recommendation_reason),
            );
            rows.push((
                readiness_sort_key(if profile.installing {
                    "Installing"
                } else {
                    "Missing"
                }),
                row,
            ));
        } else {
            let row = readiness_row(
                use_case,
                "Missing",
                "warning",
                "No profile in the library advertises this task.",
                None,
                None,
            );
            rows.push((readiness_sort_key("Missing"), row));
        }
    }

    rows.sort_by_key(|(sort_key, _)| *sort_key);
    for (_, row) in rows {
        list_box.append(&row);
    }
}

fn readiness_sort_key(status: &str) -> u8 {
    if status == "Ready" { 1 } else { 0 }
}

fn readiness_row(
    use_case: &str,
    status: &str,
    status_style: &str,
    detail: &str,
    button: Option<Button>,
    tooltip: Option<&str>,
) -> Box {
    let row = Box::new(Orientation::Horizontal, 12);
    row.set_margin_top(10);
    row.set_margin_bottom(10);
    row.set_margin_start(12);
    row.set_margin_end(12);
    row.set_tooltip_text(tooltip);

    let details = Box::new(Orientation::Vertical, 4);
    details.set_hexpand(true);

    let heading = Box::new(Orientation::Horizontal, 8);
    heading.set_valign(gtk4::Align::Center);

    let title = Label::new(Some(use_case));
    title.set_xalign(0.0);
    title.add_css_class("heading");
    heading.append(&title);

    let status_label = Label::new(Some(status));
    status_label.add_css_class("caption");
    status_label.add_css_class("pill");
    status_label.add_css_class(status_style);
    heading.append(&status_label);

    let subtitle = Label::new(Some(detail));
    subtitle.set_xalign(0.0);
    subtitle.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    subtitle.add_css_class("dim-label");

    details.append(&heading);
    details.append(&subtitle);
    row.append(&details);

    if let Some(button) = button {
        button.set_valign(gtk4::Align::Center);
        row.append(&button);
    }

    row
}

fn recommendation_button(
    candidate: &aileron_varlink::aileron_Models::CatalogProfileInfo,
    profiles: &[aileron_varlink::aileron_Models::ProfileInfo],
    use_case: &str,
    lists: &ModelLists,
) -> Button {
    let installed_better = profiles
        .iter()
        .any(|installed| installed.profile_id == candidate.profile_id);
    if installed_better {
        let button = Button::with_label("Assign better");
        button.set_valign(gtk4::Align::Center);
        let profile_id = candidate.profile_id.clone();
        let use_case = use_case.to_string();
        let lists_for_assign = lists.clone();
        button.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            assign_use_case_direct(
                profile_id.clone(),
                use_case.clone(),
                lists_for_assign.clone(),
                window,
            );
        });
        button
    } else {
        let button = Button::with_label("Install better");
        button.set_valign(gtk4::Align::Center);
        button.set_sensitive(!candidate.installing);
        let profile_id = candidate.profile_id.clone();
        let lists_for_install = lists.clone();
        button.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            install_catalog_profile(profile_id.clone(), lists_for_install.clone(), window);
        });
        button
    }
}

fn use_case_kind(use_case: &str) -> &'static str {
    if use_case.starts_with("language.") {
        "Text"
    } else if use_case.starts_with("speech.") {
        "Speech"
    } else if use_case.starts_with("vision.") {
        "Vision"
    } else {
        "Task"
    }
}

fn refresh_downloads_list(lists: &ModelLists) {
    let list = &lists.downloads;
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }

    use aileron_varlink::aileron_Models::VarlinkClientInterface;
    let installs = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client.list_installs().call().map_err(|e| e.to_string())
        });

    let installs = match installs {
        Ok(reply) => reply.installs,
        Err(reason) => {
            let row = ActionRow::new();
            row.set_title("Downloads unavailable");
            row.set_subtitle(&reason);
            list.append(&row);
            return;
        }
    };

    if installs.is_empty() {
        let row = ActionRow::new();
        row.set_title("No active downloads");
        row.set_subtitle("Install progress appears here while profiles are downloading.");
        list.append(&row);
    }

    for install in installs {
        let row = download_row(&install, lists, None);
        list.append(&row);
    }
}

fn download_row(install: &InstallStatus, lists: &ModelLists, window: Option<gtk4::Window>) -> Box {
    let row = Box::new(Orientation::Horizontal, 12);
    row.set_margin_top(10);
    row.set_margin_bottom(10);
    row.set_margin_start(12);
    row.set_margin_end(12);

    let details = Box::new(Orientation::Vertical, 6);
    details.set_hexpand(true);
    let title = Label::new(Some(&install.profile_id));
    title.set_xalign(0.0);
    title.add_css_class("heading");
    let subtitle = Label::new(Some(&download_subtitle(
        install.bytes_pulled,
        install.total_bytes,
        install.bytes_per_second,
        install.eta_seconds,
        &install.status,
        install.cancel_requested,
    )));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("dim-label");
    let progress = ProgressBar::new();
    if install.total_bytes > 0 {
        progress.set_fraction(
            (install.bytes_pulled as f64 / install.total_bytes as f64).clamp(0.0, 1.0),
        );
    } else {
        progress.pulse();
    }
    details.append(&title);
    details.append(&subtitle);
    details.append(&progress);
    row.append(&details);

    let cancel = Button::with_label("Cancel download");
    cancel.set_valign(gtk4::Align::Center);
    if install_is_terminal(install) {
        return row;
    }
    cancel.set_sensitive(!install.cancel_requested);
    let profile_id = install.profile_id.clone();
    let lists_for_cancel = lists.clone();
    cancel.connect_clicked(move |btn| {
        let window = window
            .clone()
            .or_else(|| btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok()));
        confirm_cancel_install(profile_id.clone(), lists_for_cancel.clone(), window);
    });
    row.append(&cancel);
    row
}

fn install_is_terminal(install: &InstallStatus) -> bool {
    install_is_terminal_status(&install.status)
}

fn download_subtitle(
    bytes_pulled: i64,
    total_bytes: i64,
    bytes_per_second: i64,
    eta_seconds: i64,
    status: &str,
    cancelling: bool,
) -> String {
    if status.starts_with("Failed:") {
        return status.to_string();
    }

    let prefix = if cancelling { "Cancelling" } else { status };
    let speed = if bytes_per_second > 0 {
        format!(" · {}", format_speed(bytes_per_second))
    } else {
        " · speed calculating".to_string()
    };
    let eta = if eta_seconds >= 0 {
        format!(" · {} left", format_duration(eta_seconds))
    } else {
        String::new()
    };
    if total_bytes > 0 {
        format!(
            "{} · {:.1} / {:.1} GB{}{}",
            prefix,
            bytes_pulled as f64 / 1_000_000_000.0,
            total_bytes as f64 / 1_000_000_000.0,
            speed,
            eta,
        )
    } else {
        format!("{prefix} · size unknown{speed}")
    }
}

fn cancel_install(profile_id: String, lists: ModelLists, window: Option<gtk4::Window>) {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;

    let result = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client
                .cancel_install(profile_id)
                .call()
                .map_err(|e| e.to_string())
        });

    match result {
        Ok(_) => {
            refresh_model_page(&lists);
            start_install_poll(&lists);
        }
        Err(reason) => show_message(window.as_ref(), "Cancel failed", &reason),
    }
}

fn confirm_cancel_install(profile_id: String, lists: ModelLists, window: Option<gtk4::Window>) {
    let dialog = AlertDialog::builder()
        .heading("Cancel download?")
        .body("The current profile download will stop. You can start it again later.")
        .build();
    dialog.add_response("keep", "Keep downloading");
    dialog.add_response("cancel", "Cancel download");
    dialog.set_response_appearance("cancel", libadwaita::ResponseAppearance::Destructive);
    dialog.set_close_response("keep");

    let window_for_response = window.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "cancel" {
            cancel_install(
                profile_id.clone(),
                lists.clone(),
                window_for_response.clone(),
            );
        }
    });
    dialog.present(window.as_ref());
}

fn candidate_rank(
    profile: &aileron_varlink::aileron_Models::CatalogProfileInfo,
    assigned: &HashSet<&str>,
    use_case: &str,
) -> u8 {
    if profile
        .use_cases
        .iter()
        .any(|use_case| assigned.contains(use_case.as_str()))
    {
        10
    } else if profile.recommended {
        0
    } else if matches!(use_case, "speech.transcribe" | "speech.translate")
        && profile.fit_level == "fits_minimum"
    {
        match profile.tier.as_str() {
            "balanced" => 1,
            "large" => 2,
            "small" => 3,
            _ => 4,
        }
    } else if profile.fit_level == "fits_minimum" {
        1
    } else {
        2
    }
}

fn best_missing_candidate<'a>(
    catalog: &'a [aileron_varlink::aileron_Models::CatalogProfileInfo],
    assigned: &HashSet<&str>,
    use_case: &str,
) -> Option<&'a aileron_varlink::aileron_Models::CatalogProfileInfo> {
    catalog
        .iter()
        .filter(|profile| profile.use_cases.iter().any(|uc| uc == use_case))
        .min_by(|a, b| compare_candidates(a, b, assigned, use_case))
}

fn better_catalog_candidate<'a>(
    catalog: &'a [aileron_varlink::aileron_Models::CatalogProfileInfo],
    assigned_profile_id: &str,
    use_case: &str,
) -> Option<&'a aileron_varlink::aileron_Models::CatalogProfileInfo> {
    let empty_assigned = HashSet::new();
    let best = best_missing_candidate(catalog, &empty_assigned, use_case)?;
    if best.profile_id == assigned_profile_id {
        return None;
    }

    let Some(current) = catalog
        .iter()
        .find(|profile| profile.profile_id == assigned_profile_id)
    else {
        return Some(best);
    };

    (compare_candidates(best, current, &empty_assigned, use_case) == std::cmp::Ordering::Less)
        .then_some(best)
}

fn compare_candidates(
    a: &aileron_varlink::aileron_Models::CatalogProfileInfo,
    b: &aileron_varlink::aileron_Models::CatalogProfileInfo,
    assigned: &HashSet<&str>,
    use_case: &str,
) -> std::cmp::Ordering {
    candidate_rank(a, assigned, use_case)
        .cmp(&candidate_rank(b, assigned, use_case))
        .then_with(|| compare_fit_score(task_fit_score(a, use_case), task_fit_score(b, use_case)))
        .then_with(|| compare_asr_quality(a, b, use_case))
        .then_with(|| a.disk_size_gb.total_cmp(&b.disk_size_gb))
}

fn task_fit_score(
    profile: &aileron_varlink::aileron_Models::CatalogProfileInfo,
    use_case: &str,
) -> f64 {
    profile
        .use_case_fit_scores
        .iter()
        .find(|fit| fit.use_case == use_case)
        .map(|fit| fit.score)
        .unwrap_or(profile.fit_score)
}

fn compare_fit_score(a: f64, b: f64) -> std::cmp::Ordering {
    if a > 0.0 && b > 0.0 {
        b.total_cmp(&a)
    } else {
        std::cmp::Ordering::Equal
    }
}

fn compare_asr_quality(
    a: &aileron_varlink::aileron_Models::CatalogProfileInfo,
    b: &aileron_varlink::aileron_Models::CatalogProfileInfo,
    use_case: &str,
) -> std::cmp::Ordering {
    if matches!(use_case, "speech.transcribe" | "speech.translate") {
        asr_quality_rank(a).cmp(&asr_quality_rank(b))
    } else {
        std::cmp::Ordering::Equal
    }
}

fn asr_quality_rank(profile: &aileron_varlink::aileron_Models::CatalogProfileInfo) -> u8 {
    let id = profile.profile_id.as_str();
    if id.contains("large-v3-turbo") {
        0
    } else if id.contains("large-v3") {
        1
    } else if id.contains("medium") {
        2
    } else if id.contains("small") {
        3
    } else {
        4
    }
}

fn assign_use_case_direct(
    profile_id: String,
    use_case: String,
    lists: ModelLists,
    window: Option<gtk4::Window>,
) {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;

    let result = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client
                .assign_use_case(profile_id, use_case)
                .call()
                .map_err(|e| e.to_string())
        });

    match result {
        Ok(_) => refresh_model_page(&lists),
        Err(reason) => show_message(window.as_ref(), "Assign failed", &reason),
    }
}

fn refresh_model_list(lists: &ModelLists) {
    let list_box = &lists.profiles;
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
                row.set_subtitle("Use the Tasks or Profile Library tab to install a profile.");
                list_box.append(&row);
                return;
            }
            for model in &reply.profiles {
                let row = ActionRow::new();
                row.set_title(&model.profile_id);
                let availability = profile_availability(&model.assigned_use_cases);
                row.set_subtitle(&format!(
                    "{} · {} · {} · {} · {}",
                    availability,
                    model_kind(&model.runtime_id),
                    source_label(&model.source),
                    assignment_count(&model.assigned_use_cases),
                    format_profile_size(model.size_bytes)
                ));

                let details_btn = Button::with_label("Details");
                details_btn.set_valign(gtk4::Align::Center);
                let details = ProfileDetails {
                    profile_id: model.profile_id.clone(),
                    model_id: model.model_id.clone(),
                    runtime_id: model.runtime_id.clone(),
                    artifact_path: model.artifact_path.clone(),
                    source: source_label(&model.source).to_string(),
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
                let supported_use_cases = model.use_cases.clone();
                let current_use_cases = model.assigned_use_cases.clone();
                let lists_assign = lists.clone();
                assign_btn.connect_clicked(move |btn| {
                    let dialog = AlertDialog::builder()
                        .heading("Assign use-cases")
                        .body(format!("Select use-cases for:\n{}", profile_id_assign))
                        .build();
                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("assign", "Assign");
                    dialog.set_response_appearance(
                        "assign",
                        libadwaita::ResponseAppearance::Suggested,
                    );
                    dialog.set_default_response(Some("assign"));
                    dialog.set_close_response("cancel");

                    // Build a checkbox for each use-case this profile declares.
                    let vbox = Box::new(Orientation::Vertical, 4);
                    vbox.set_margin_top(12);
                    let checkboxes: Vec<(CheckButton, String)> = supported_use_cases
                        .iter()
                        .map(|uc| {
                            let cb = CheckButton::with_label(uc);
                            cb.set_active(current_use_cases.contains(uc));
                            vbox.append(&cb);
                            (cb, uc.clone())
                        })
                        .collect();
                    dialog.set_extra_child(Some(&vbox));

                    let profile_id2 = profile_id_assign.clone();
                    let lists2 = lists_assign.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response != "assign" {
                            return;
                        }
                        let selected: Vec<String> = checkboxes
                            .iter()
                            .filter(|(cb, _)| cb.is_active())
                            .map(|(_, uc)| uc.clone())
                            .collect();
                        let profile_id3 = profile_id2.clone();
                        let lists3 = lists2.clone();
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
                            refresh_model_page(&lists3);
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
                delete_btn.set_sensitive(model.source != "system");
                if model.source == "system" {
                    delete_btn.set_tooltip_text(Some(
                        "System-backed profiles are managed by distro packages.",
                    ));
                }
                let profile_id = model.profile_id.clone();
                let lists_ref = lists.clone();
                delete_btn.connect_clicked(move |btn| {
                    let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
                    confirm_delete_profile(profile_id.clone(), lists_ref.clone(), window);
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

fn confirm_delete_profile(profile_id: String, lists: ModelLists, window: Option<gtk4::Window>) {
    let dialog = AlertDialog::builder()
        .heading("Delete profile?")
        .body("This removes the installed profile from Aileron.")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete");
    dialog.set_response_appearance("delete", libadwaita::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");

    let window_for_response = window.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "delete" {
            delete_profile(
                profile_id.clone(),
                false,
                lists.clone(),
                window_for_response.clone(),
            );
        }
    });
    dialog.present(window.as_ref());
}

/// Present conflict resolution only when assigning tasks requires a user decision.
fn show_pull_result_dialog(
    window: Option<&gtk4::Window>,
    auto_assigned: Vec<String>,
    conflicts: Vec<aileron_varlink::aileron_Models::UseCaseConflict>,
    lists: ModelLists,
) {
    if conflicts.is_empty() {
        return;
    }

    let dialog = AlertDialog::builder()
        .heading("Reassign tasks?")
        .body("Some tasks are already assigned to another profile. Choose whether to keep the current assignments or move them to the newly installed profile.")
        .build();
    dialog.add_response("keep", "Keep current assignments");
    dialog.add_response("reassign", "Reassign tasks");
    dialog.set_response_appearance("reassign", libadwaita::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("keep"));
    dialog.set_close_response("keep");

    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(6);

    if !auto_assigned.is_empty() {
        let assigned_group = PreferencesGroup::new();
        assigned_group.set_title("Assigned automatically");
        assigned_group.set_description(Some(
            "These tasks were assigned to the installed profile and need no action.",
        ));
        for use_case in &auto_assigned {
            let row = ActionRow::new();
            row.set_title(use_case);
            assigned_group.add(&row);
        }
        vbox.append(&assigned_group);
    }

    let conflict_group = PreferencesGroup::new();
    conflict_group.set_title("Needs review");
    for c in &conflicts {
        let row = ActionRow::new();
        row.set_title(&c.use_case);
        row.set_subtitle(&format!(
            "Current: {} · New: {}",
            c.current_profile, c.new_profile
        ));
        conflict_group.add(&row);
    }
    vbox.append(&conflict_group);
    dialog.set_extra_child(Some(&vbox));

    dialog.connect_response(None, move |_, response| {
        if response != "reassign" {
            return;
        }
        let conflicts_clone = conflicts.clone();
        let lists_clone = lists.clone();
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
            refresh_model_page(&lists_clone);
        });
    });

    dialog.present(window);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[test]
    fn formats_install_failed_reason() {
        let message = install_error_message(
            "aileron.Models.InstallFailed: Some(InstallFailed_Args { profile_id: \"x\", reason: \"local runtime image is not built: localhost/example:cpu\", })",
        );

        assert!(message.contains("The required local runtime image is missing"));
        assert!(message.contains("localhost/example:cpu"));
        assert!(!message.contains("InstallFailed_Args"));
    }

    #[hegel::test]
    fn extracts_generated_varlink_reason(tc: TestCase) {
        let reason = tc.draw(gs::sampled_from(vec![
            "install cancelled".to_string(),
            "local runtime image is not built: localhost/example:cpu".to_string(),
            "runtime image download already running".to_string(),
        ]));
        let message = format!(
            "aileron.Models.InstallFailed: Some(InstallFailed_Args {{ profile_id: \"x\", reason: \"{reason}\", }})"
        );

        assert_eq!(extract_varlink_reason(&message), Some(reason.as_str()));
    }

    #[test]
    fn asr_recommendation_prefers_large_v3_turbo_over_smaller_medium() {
        let assigned = HashSet::new();
        let turbo = catalog_profile("whisper-large-v3-turbo-q5-0", "balanced", 0.54);
        let medium = catalog_profile("whisper-medium-q5-0", "balanced", 0.50);

        assert_eq!(
            compare_candidates(&turbo, &medium, &assigned, "speech.transcribe"),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn fit_score_label_is_neutral() {
        assert_eq!(fit_score_label(86.2).as_deref(), Some("86/100"));
        assert_eq!(fit_score_label(0.0), None);
    }

    #[hegel::test]
    fn fit_score_label_matches_generated_positive_rule(tc: TestCase) {
        let tenths = tc.draw(gs::integers::<i64>().min_value(-1000).max_value(1000));
        let score = tenths as f64 / 10.0;

        assert_eq!(
            fit_score_label(score),
            (score > 0.0).then(|| format!("{score:.0}/100"))
        );
    }

    #[test]
    fn formats_small_profile_sizes_as_mb() {
        assert_eq!(format_size(0.0177), "18 MB");
        assert_eq!(format_size(1.88), "1.9 GB");
    }

    #[test]
    fn formats_installed_profile_sizes_from_bytes() {
        assert_eq!(format_profile_size(0), "unknown size");
        assert_eq!(format_profile_size(512), "512 B");
        assert_eq!(format_profile_size(18 * 1024 * 1024), "18 MB");
        assert_eq!(format_profile_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[hegel::test]
    fn assignment_count_matches_generated_count(tc: TestCase) {
        let count = tc.draw(gs::integers::<usize>().max_value(5));
        let use_cases = (0..count)
            .map(|index| format!("language.generated.{index}"))
            .collect::<Vec<_>>();

        assert_eq!(
            assignment_count(&use_cases),
            match count {
                0 => "Unassigned".to_string(),
                1 => "1 use-case".to_string(),
                count => format!("{count} use-cases"),
            }
        );
    }

    #[hegel::test]
    fn join_or_none_matches_generated_values(tc: TestCase) {
        let values = tc.draw(
            gs::vecs(gs::sampled_from(vec![
                "language.summarize".to_string(),
                "speech.transcribe".to_string(),
                "vision.describe".to_string(),
            ]))
            .max_size(4),
        );

        assert_eq!(
            join_or_none(&values),
            if values.is_empty() {
                "none".to_string()
            } else {
                values.join(", ")
            }
        );
    }

    #[hegel::test]
    fn formats_generated_non_positive_profile_sizes_as_unknown(tc: TestCase) {
        let bytes = tc.draw(gs::integers::<i64>().max_value(0));

        assert_eq!(format_profile_size(bytes), "unknown size");
    }

    #[test]
    fn readiness_sort_key_bubbles_non_ready_tasks() {
        assert!(readiness_sort_key("Missing") < readiness_sort_key("Ready"));
        assert!(readiness_sort_key("Installed") < readiness_sort_key("Ready"));
        assert!(readiness_sort_key("Installing") < readiness_sort_key("Ready"));
        assert_eq!(readiness_sort_key("Ready"), 1);
    }

    #[test]
    fn recommendation_uses_task_specific_fit_score() {
        let assigned = HashSet::new();
        let mut chat_better = catalog_profile("chat-better", "balanced", 2.0);
        chat_better.recommended = true;
        chat_better.fit_level = "recommended".to_string();
        chat_better.fit_score = 50.0;
        chat_better.use_cases = vec![
            "language.summarize".to_string(),
            "language.analyze".to_string(),
        ];
        chat_better.use_case_fit_scores = vec![
            aileron_varlink::aileron_Models::UseCaseFitScore {
                use_case: "language.summarize".to_string(),
                score: 90.0,
            },
            aileron_varlink::aileron_Models::UseCaseFitScore {
                use_case: "language.analyze".to_string(),
                score: 60.0,
            },
        ];

        let mut reasoning_better = catalog_profile("reasoning-better", "balanced", 2.0);
        reasoning_better.recommended = true;
        reasoning_better.fit_level = "recommended".to_string();
        reasoning_better.fit_score = 50.0;
        reasoning_better.use_cases = vec![
            "language.summarize".to_string(),
            "language.analyze".to_string(),
        ];
        reasoning_better.use_case_fit_scores = vec![
            aileron_varlink::aileron_Models::UseCaseFitScore {
                use_case: "language.summarize".to_string(),
                score: 70.0,
            },
            aileron_varlink::aileron_Models::UseCaseFitScore {
                use_case: "language.analyze".to_string(),
                score: 95.0,
            },
        ];

        assert_eq!(
            compare_candidates(
                &chat_better,
                &reasoning_better,
                &assigned,
                "language.summarize"
            ),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_candidates(
                &chat_better,
                &reasoning_better,
                &assigned,
                "language.analyze"
            ),
            std::cmp::Ordering::Greater
        );
    }

    #[test]
    fn ready_task_can_surface_better_catalog_candidate() {
        let mut current = catalog_profile("current", "balanced", 2.0);
        current.recommended = true;
        current.fit_level = "recommended".to_string();
        current.fit_score = 70.0;
        current.use_cases = vec!["language.summarize".to_string()];
        current.use_case_fit_scores = vec![aileron_varlink::aileron_Models::UseCaseFitScore {
            use_case: "language.summarize".to_string(),
            score: 70.0,
        }];

        let mut better = catalog_profile("better", "balanced", 5.0);
        better.recommended = true;
        better.fit_level = "recommended".to_string();
        better.fit_score = 90.0;
        better.use_cases = vec!["language.summarize".to_string()];
        better.use_case_fit_scores = vec![aileron_varlink::aileron_Models::UseCaseFitScore {
            use_case: "language.summarize".to_string(),
            score: 90.0,
        }];

        let catalog = vec![current, better];

        assert_eq!(
            better_catalog_candidate(&catalog, "current", "language.summarize")
                .map(|profile| profile.profile_id.as_str()),
            Some("better")
        );
    }

    #[test]
    fn installed_unassigned_task_can_still_surface_better_candidate() {
        let mut installed = catalog_profile("installed", "balanced", 2.0);
        installed.recommended = true;
        installed.fit_level = "recommended".to_string();
        installed.fit_score = 60.0;
        installed.use_cases = vec!["language.summarize".to_string()];

        let mut better = catalog_profile("better", "balanced", 5.0);
        better.recommended = true;
        better.fit_level = "recommended".to_string();
        better.fit_score = 90.0;
        better.use_cases = vec!["language.summarize".to_string()];

        let catalog = vec![installed, better];

        assert_eq!(
            better_catalog_candidate(&catalog, "installed", "language.summarize")
                .map(|profile| profile.profile_id.as_str()),
            Some("better")
        );
    }

    fn catalog_profile(
        profile_id: &str,
        tier: &str,
        disk_size_gb: f64,
    ) -> aileron_varlink::aileron_Models::CatalogProfileInfo {
        aileron_varlink::aileron_Models::CatalogProfileInfo {
            profile_id: profile_id.to_string(),
            model_id: profile_id.to_string(),
            llmfit_model_id: String::new(),
            spdx_license: Some(String::new()),
            runtime_id: "asr-whisper-cpp".to_string(),
            tier: tier.to_string(),
            disk_size_gb,
            min_ram_gb: 1.0,
            recommended_ram_gb: 1.0,
            min_vram_gb: 0.0,
            fit_score: 0.0,
            use_case_fit_scores: Vec::new(),
            fit_level: "fits_minimum".to_string(),
            recommended: false,
            installing: false,
            recommendation_reason: String::new(),
            use_cases: vec!["speech.transcribe".to_string()],
            specializations: Some(Vec::new()),
        }
    }
}
