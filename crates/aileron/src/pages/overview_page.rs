use aileron_varlink::aileron_Models::InstallStatus;
use gtk4::prelude::*;
use gtk4::{Box, Button, Label, ListBox, Orientation, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, PreferencesGroup, PreferencesPage};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

const USE_CASES: &[&str] = &[
    "language.summarize",
    "language.translate",
    "language.rephrase",
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

#[derive(Default)]
pub struct OverviewPage {
    summary: OverviewSummary,
}

#[derive(Debug)]
pub enum OverviewMsg {
    Refresh,
}

pub struct OverviewWidgets {
    readiness_value: Label,
    downloads_value: Label,
    runtimes_value: Label,
    sessions_value: Label,
    details: ListBox,
}

#[derive(Clone, Debug, Default)]
pub struct OverviewSummary {
    ready_tasks: usize,
    active_downloads: usize,
    runtime_images: usize,
    active_sessions: usize,
    permissions: usize,
    errors: Vec<String>,
}

impl SimpleComponent for OverviewPage {
    type Init = ();
    type Input = OverviewMsg;
    type Output = ();
    type Widgets = OverviewWidgets;
    type Root = PreferencesPage;

    fn init_root() -> Self::Root {
        PreferencesPage::new()
    }

    fn init(
        (): Self::Init,
        page: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let widgets = build_page(&page, sender.clone());
        sender.input(OverviewMsg::Refresh);
        ComponentParts {
            model: OverviewPage::default(),
            widgets,
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            OverviewMsg::Refresh => self.summary = load_summary(),
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        widgets.readiness_value.set_text(&format!(
            "{} / {}",
            self.summary.ready_tasks,
            USE_CASES.len()
        ));
        widgets
            .downloads_value
            .set_text(&self.summary.active_downloads.to_string());
        widgets
            .runtimes_value
            .set_text(&self.summary.runtime_images.to_string());
        widgets
            .sessions_value
            .set_text(&self.summary.active_sessions.to_string());

        clear_list(&widgets.details);
        if self.summary.errors.is_empty() {
            append_detail(
                &widgets.details,
                "System reachable",
                "Daemon queries completed. Use the pages in the sidebar for detailed actions.",
            );
            append_detail(
                &widgets.details,
                "Permissions recorded",
                &format!(
                    "{} app/use-case permission entries",
                    self.summary.permissions
                ),
            );
        } else {
            for error in &self.summary.errors {
                append_detail(&widgets.details, "Needs attention", error);
            }
        }
    }
}

fn build_page(page: &PreferencesPage, sender: ComponentSender<OverviewPage>) -> OverviewWidgets {
    let group = PreferencesGroup::new();
    group.set_title("Operations Overview");
    group.set_description(Some(
        "A status-first snapshot of model readiness, downloads, runtimes, permissions, and sessions.",
    ));

    let refresh = Button::with_label("Refresh");
    refresh.set_valign(gtk4::Align::Center);
    refresh.connect_clicked(move |_| sender.input(OverviewMsg::Refresh));
    group.set_header_suffix(Some(&refresh));

    let grid = Box::new(Orientation::Vertical, 12);
    grid.set_margin_top(12);
    grid.set_margin_bottom(12);
    grid.set_margin_start(12);
    grid.set_margin_end(12);

    let metrics = Box::new(Orientation::Horizontal, 12);
    metrics.set_homogeneous(true);
    let (readiness_card, readiness_value) = metric_card("Task readiness", "0 / 12");
    let (downloads_card, downloads_value) = metric_card("Downloads", "0");
    let (runtimes_card, runtimes_value) = metric_card("Runtime images", "0");
    let (sessions_card, sessions_value) = metric_card("Active sessions", "0");
    metrics.append(&readiness_card);
    metrics.append(&downloads_card);
    metrics.append(&runtimes_card);
    metrics.append(&sessions_card);
    grid.append(&metrics);

    let details = ListBox::new();
    details.set_selection_mode(gtk4::SelectionMode::None);
    details.add_css_class("boxed-list");
    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(220)
        .child(&details)
        .build();
    grid.append(&scroll);
    group.add(&grid);
    page.add(&group);

    OverviewWidgets {
        readiness_value,
        downloads_value,
        runtimes_value,
        sessions_value,
        details,
    }
}

fn metric_card(title: &str, value: &str) -> (Box, Label) {
    let card = Box::new(Orientation::Vertical, 4);
    card.add_css_class("card");
    card.set_height_request(96);
    card.set_margin_top(4);
    card.set_margin_bottom(4);
    card.set_margin_start(4);
    card.set_margin_end(4);

    let content = Box::new(Orientation::Vertical, 6);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(14);
    content.set_margin_end(14);

    let title_label = Label::new(Some(title));
    title_label.set_xalign(0.0);
    title_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    title_label.add_css_class("dim-label");

    let value_label = Label::new(Some(value));
    value_label.set_xalign(0.0);
    value_label.add_css_class("title-1");

    content.append(&title_label);
    content.append(&value_label);
    card.append(&content);
    (card, value_label)
}

fn load_summary() -> OverviewSummary {
    let mut summary = OverviewSummary::default();

    use aileron_varlink::aileron_Models::VarlinkClientInterface as ModelsClient;
    match aileron_ipc::client::connect() {
        Ok(conn) => {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            match client.list().call() {
                Ok(reply) => {
                    summary.ready_tasks = ready_task_count(&reply.profiles);
                }
                Err(e) => summary.errors.push(format!("Profiles unavailable: {e}")),
            }
            match client.list_installs().call() {
                Ok(reply) => {
                    summary.active_downloads = reply
                        .installs
                        .iter()
                        .filter(|install| !install_is_terminal(install))
                        .count();
                }
                Err(e) => summary.errors.push(format!("Downloads unavailable: {e}")),
            }
            match client.list_runtime_images().call() {
                Ok(reply) => summary.runtime_images = reply.images.len(),
                Err(e) => summary
                    .errors
                    .push(format!("Runtime images unavailable: {e}")),
            }
        }
        Err(e) => {
            summary.errors.push(format!("Daemon not reachable: {e}"));
            return summary;
        }
    }

    use aileron_varlink::aileron_Sessions::VarlinkClientInterface as SessionsClient;
    match aileron_ipc::client::connect() {
        Ok(conn) => {
            let mut client = aileron_varlink::aileron_Sessions::VarlinkClient::new(conn);
            match client.list_active().call() {
                Ok(reply) => summary.active_sessions = reply.sessions.len(),
                Err(e) => summary.errors.push(format!("Sessions unavailable: {e}")),
            }
        }
        Err(e) => summary
            .errors
            .push(format!("Session service unavailable: {e}")),
    }

    use aileron_varlink::aileron_Permissions::VarlinkClientInterface as PermissionsClient;
    match aileron_ipc::client::connect() {
        Ok(conn) => {
            let mut client = aileron_varlink::aileron_Permissions::VarlinkClient::new(conn);
            match client.list_app_permissions().call() {
                Ok(reply) => summary.permissions = reply.permissions.len(),
                Err(e) => summary.errors.push(format!("Permissions unavailable: {e}")),
            }
        }
        Err(e) => summary
            .errors
            .push(format!("Permission service unavailable: {e}")),
    }

    summary
}

fn install_is_terminal(install: &InstallStatus) -> bool {
    install.status.starts_with("Failed:") || install.status == "Completed"
}

fn ready_task_count(profiles: &[aileron_varlink::aileron_Models::ProfileInfo]) -> usize {
    USE_CASES
        .iter()
        .filter(|use_case| {
            profiles.iter().any(|profile| {
                profile
                    .use_cases
                    .iter()
                    .any(|supported| supported == **use_case)
                    && profile
                        .assigned_use_cases
                        .iter()
                        .any(|assigned| assigned == **use_case)
            })
        })
        .count()
}

fn append_detail(list: &ListBox, title: &str, subtitle: &str) {
    let row = ActionRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    list.append(&row);
}

fn clear_list(list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aileron_varlink::aileron_Models::ProfileInfo;

    fn profile(use_cases: &[&str], assigned_use_cases: &[&str]) -> ProfileInfo {
        ProfileInfo {
            profile_id: "profile".to_string(),
            model_id: "model".to_string(),
            runtime_id: "runtime".to_string(),
            artifact_path: "/tmp/model".to_string(),
            use_cases: use_cases.iter().map(|value| value.to_string()).collect(),
            specializations: Some(Vec::new()),
            runtime_images: Vec::new(),
            assigned_use_cases: assigned_use_cases
                .iter()
                .map(|value| value.to_string())
                .collect(),
            installed_at: String::new(),
            size_bytes: 0,
            source: "user".to_string(),
        }
    }

    #[test]
    fn ready_task_count_requires_assignment_and_profile_support() {
        let profiles = vec![profile(
            &["speech.transcribe"],
            &["speech.transcribe", "speech.translate"],
        )];

        assert_eq!(ready_task_count(&profiles), 1);
    }
}
