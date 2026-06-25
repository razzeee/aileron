use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use aileron_varlink::aileron_Models::InstallStatus;
use gtk4::pango;
use gtk4::prelude::*;
use gtk4::{Box, Button, Label, ListBox, Orientation, ProgressBar, ScrolledWindow, Spinner};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, AlertDialog, PreferencesGroup, PreferencesPage};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use super::{format_duration, format_speed, install_is_terminal_status};

pub struct DownloadsPage {
    poll_active: Rc<Cell<bool>>,
    start_poll: bool,
}

#[derive(Debug)]
pub enum DownloadsMsg {
    Refresh,
}

pub struct DownloadsWidgets {
    list_box: ListBox,
}

impl SimpleComponent for DownloadsPage {
    type Init = ();
    type Input = DownloadsMsg;
    type Output = ();
    type Widgets = DownloadsWidgets;
    type Root = PreferencesPage;

    fn init_root() -> Self::Root {
        PreferencesPage::new()
    }

    fn init(
        (): Self::Init,
        page: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let list_box = build_page(&page);
        refresh_downloads_list(&list_box);
        let model = DownloadsPage {
            poll_active: Rc::new(Cell::new(false)),
            start_poll: has_active_downloads(),
        };
        let mut widgets = DownloadsWidgets { list_box };
        model.update_view(&mut widgets, sender);
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            DownloadsMsg::Refresh => {
                self.start_poll = has_active_downloads();
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, sender: ComponentSender<Self>) {
        refresh_downloads_list(&widgets.list_box);
        if self.start_poll {
            start_poll(&widgets.list_box, self.poll_active.clone(), sender);
        }
    }
}

fn start_poll(
    list_box: &ListBox,
    poll_active: Rc<Cell<bool>>,
    sender: ComponentSender<DownloadsPage>,
) {
    if poll_active.get() {
        return;
    }
    poll_active.set(true);
    refresh_downloads_list(list_box);

    let mut grace_ticks = 15;
    glib::timeout_add_seconds_local(2, move || {
        sender.input(DownloadsMsg::Refresh);
        if has_active_downloads() {
            grace_ticks = 15;
            glib::ControlFlow::Continue
        } else if grace_ticks > 0 {
            grace_ticks -= 1;
            glib::ControlFlow::Continue
        } else {
            poll_active.set(false);
            glib::ControlFlow::Break
        }
    });
}

fn build_page(page: &PreferencesPage) -> ListBox {
    let group = PreferencesGroup::new();
    group.set_title("Downloads");
    group.set_description(Some(
        "Active profile installs, model artifact downloads, and runtime image pulls.",
    ));

    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(300)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&list_box)
        .build();
    group.add(&scroll);
    page.add(&group);
    list_box
}

fn refresh_downloads_list(list: &ListBox) {
    clear_list(list);

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

    let installs = installs
        .into_iter()
        .filter(|install| install.status != "Completed")
        .collect::<Vec<_>>();

    if installs.is_empty() {
        let row = ActionRow::new();
        row.set_title("No active downloads");
        row.set_subtitle(
            "Install and runtime image progress appears here while downloads are running.",
        );
        list.append(&row);
        return;
    }

    let (profile_installs, runtime_installs): (Vec<_>, Vec<_>) = installs
        .into_iter()
        .partition(|install| !is_runtime_download(&install.profile_id));

    let profile_runtime_ids = catalog_profile_runtime_ids();
    let mut grouped_runtime_downloads = HashSet::new();

    for install in &profile_installs {
        list.append(&profile_download_row(install, None, None));
        for runtime_install in
            matching_runtime_installs(install, &runtime_installs, &profile_runtime_ids)
        {
            grouped_runtime_downloads.insert(runtime_install.profile_id.clone());
            list.append(&runtime_setup_row(runtime_install, true));
        }
    }
    for install in &runtime_installs {
        if !grouped_runtime_downloads.contains(&install.profile_id) {
            list.append(&runtime_setup_row(install, false));
        }
    }
}

fn profile_download_row(
    install: &InstallStatus,
    runtime_install: Option<&InstallStatus>,
    window: Option<gtk4::Window>,
) -> Box {
    let row = Box::new(Orientation::Horizontal, 12);
    row.set_margin_top(10);
    row.set_margin_bottom(10);
    row.set_margin_start(12);
    row.set_margin_end(12);

    let details = Box::new(Orientation::Vertical, 6);
    details.set_hexpand(true);
    details.set_halign(gtk4::Align::Fill);

    let title = Label::new(Some(&download_title(&install.profile_id)));
    configure_download_label(&title, 48);
    title.add_css_class("heading");

    let subtitle = Label::new(Some(&download_subtitle(
        install.bytes_pulled,
        install.total_bytes,
        install.bytes_per_second,
        install.eta_seconds,
        &install.status,
        install.cancel_requested,
        runtime_install,
    )));
    configure_download_label(&subtitle, 64);
    subtitle.add_css_class("dim-label");

    details.append(&title);
    details.append(&subtitle);
    if let Some(runtime_install) = runtime_install
        && runtime_install.total_bytes > 0
    {
        let progress = ProgressBar::new();
        progress.set_hexpand(true);
        progress.set_fraction(
            (runtime_install.bytes_pulled as f64 / runtime_install.total_bytes as f64)
                .clamp(0.0, 1.0),
        );
        details.append(&progress);
    } else if install.total_bytes > 0 && !is_runtime_setup_status(&install.status) {
        let progress = ProgressBar::new();
        progress.set_hexpand(true);
        progress.set_fraction(
            (install.bytes_pulled as f64 / install.total_bytes as f64).clamp(0.0, 1.0),
        );
        details.append(&progress);
    }
    row.append(&details);

    let runtime_is_indeterminate = runtime_install
        .map(|runtime_install| runtime_install.total_bytes <= 0)
        .unwrap_or(false);
    if ((install.total_bytes <= 0 || is_runtime_setup_status(&install.status))
        || runtime_is_indeterminate)
        && !install_is_terminal(install)
    {
        let spinner = Spinner::new();
        spinner.set_valign(gtk4::Align::Center);
        spinner.start();
        row.append(&spinner);
    }

    if install_is_terminal(install) {
        return row;
    }

    let cancel = Button::with_label("Cancel download");
    cancel.set_valign(gtk4::Align::Center);
    cancel.set_sensitive(!install.cancel_requested);
    let profile_id = install.profile_id.clone();
    cancel.connect_clicked(move |btn| {
        let window = window
            .clone()
            .or_else(|| btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok()));
        confirm_cancel_install(&profile_id, window);
    });
    row.append(&cancel);
    row
}

fn runtime_setup_row(install: &InstallStatus, grouped: bool) -> Box {
    let row = Box::new(Orientation::Horizontal, 12);
    row.set_margin_top(if grouped { 6 } else { 8 });
    row.set_margin_bottom(10);
    row.set_margin_start(if grouped { 54 } else { 12 });
    row.set_margin_end(12);

    if !grouped {
        let spinner = Spinner::new();
        spinner.set_valign(gtk4::Align::Center);
        spinner.start();
        row.append(&spinner);
    }

    let details = Box::new(Orientation::Vertical, 5);
    details.set_hexpand(true);
    details.set_halign(gtk4::Align::Fill);

    let title = Label::new(Some(&runtime_setup_title(install)));
    configure_download_label(&title, 48);
    if !grouped {
        title.add_css_class("heading");
    }

    let subtitle = Label::new(Some(&runtime_detail_line(install)));
    configure_download_label(&subtitle, 64);
    subtitle.add_css_class("dim-label");

    details.append(&title);
    details.append(&subtitle);

    row.append(&details);
    row
}

fn configure_download_label(label: &Label, max_width_chars: i32) {
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_halign(gtk4::Align::Fill);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.set_max_width_chars(max_width_chars);
}

fn download_title(id: &str) -> String {
    id.strip_prefix("runtime:")
        .map(runtime_title)
        .unwrap_or_else(|| id.to_string())
}

fn runtime_title(image_ref: &str) -> String {
    format!("Runtime environment: {}", runtime_name(image_ref))
}

fn is_runtime_download(id: &str) -> bool {
    id.starts_with("runtime:")
}

fn matching_runtime_installs<'a>(
    profile_install: &InstallStatus,
    runtime_installs: &'a [InstallStatus],
    profile_runtime_ids: &HashMap<String, String>,
) -> Vec<&'a InstallStatus> {
    if !is_runtime_setup_status(&profile_install.status) {
        return Vec::new();
    }
    let Some(runtime_id) = profile_runtime_ids.get(&profile_install.profile_id) else {
        return Vec::new();
    };

    runtime_installs
        .iter()
        .filter(|install| {
            runtime_download_runtime_id(&install.profile_id).as_deref() == Some(runtime_id.as_str())
        })
        .collect()
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
    runtime_install: Option<&InstallStatus>,
) -> String {
    if status.starts_with("Failed:") {
        return status.to_string();
    }
    if let Some(runtime_install) = runtime_install {
        return if cancelling {
            "Cancelling runtime setup".to_string()
        } else {
            runtime_profile_subtitle(runtime_install)
        };
    }
    if is_runtime_setup_status(status) {
        return if cancelling {
            "Cancelling runtime setup".to_string()
        } else {
            "Setting up runtime environment before model download".to_string()
        };
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

fn runtime_profile_subtitle(install: &InstallStatus) -> String {
    let image_ref = runtime_download_image_ref(&install.profile_id);
    let phase = runtime_phase(&install.status);
    let progress = if install.total_bytes > 0 {
        format!(
            " · {:.0}%",
            (install.bytes_pulled as f64 / install.total_bytes as f64 * 100.0).clamp(0.0, 100.0)
        )
    } else {
        String::new()
    };
    format!("{phase} {}{progress}", runtime_name(image_ref))
}

fn is_runtime_setup_status(status: &str) -> bool {
    status.contains("runtime image")
}

fn runtime_setup_title(install: &InstallStatus) -> String {
    let image_ref = runtime_download_image_ref(&install.profile_id);
    let phase = runtime_phase(&install.status);
    format!("{phase} {}", runtime_name(image_ref))
}

fn runtime_detail_line(install: &InstallStatus) -> String {
    let image_ref = runtime_download_image_ref(&install.profile_id);
    if install.status.starts_with("Failed:") {
        format!("{} · {}", install.status, compact_image_ref(image_ref))
    } else {
        compact_image_ref(image_ref)
    }
}

fn runtime_download_image_ref(profile_id: &str) -> &str {
    profile_id.strip_prefix("runtime:").unwrap_or(profile_id)
}

fn runtime_download_runtime_id(profile_id: &str) -> Option<String> {
    let image_ref = profile_id.strip_prefix("runtime:")?;
    let image = image_ref
        .rsplit_once('/')
        .map_or(image_ref, |(_, image)| image);
    let name = image.rsplit_once(':').map_or(image, |(name, _)| name);
    Some(
        name.strip_prefix("aileron-runtime-")
            .unwrap_or(name)
            .to_string(),
    )
}

fn catalog_profile_runtime_ids() -> HashMap<String, String> {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;

    aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client.list_catalog().call().map_err(|e| e.to_string())
        })
        .map(|reply| {
            reply
                .profiles
                .into_iter()
                .map(|profile| (profile.profile_id, profile.runtime_id))
                .collect()
        })
        .unwrap_or_default()
}

fn runtime_phase(status: &str) -> &'static str {
    if status.starts_with("Failed:") {
        "Failed to prepare"
    } else if status.contains("Pulling") {
        "Pulling"
    } else if status.contains("Unpacking") || status.contains("unpack") {
        "Unpacking"
    } else {
        "Preparing"
    }
}

fn runtime_name(image_ref: &str) -> String {
    let image = image_ref
        .rsplit_once('/')
        .map_or(image_ref, |(_, image)| image);
    let (name, variant) = image.rsplit_once(':').unwrap_or((image, "runtime"));
    let name = name.strip_prefix("aileron-runtime-").unwrap_or(name);
    format!("{} runtime ({variant})", name.replace('-', " "))
}

fn compact_image_ref(image_ref: &str) -> String {
    let Some((registry, rest)) = image_ref.split_once('/') else {
        return image_ref.to_string();
    };
    let Some((_, image)) = rest.rsplit_once('/') else {
        return image_ref.to_string();
    };
    format!("{registry}/…/{image}")
}

fn has_active_downloads() -> bool {
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

fn cancel_install(profile_id: &str, window: Option<gtk4::Window>) {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;

    let result = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client
                .cancel_install(profile_id.to_string())
                .call()
                .map_err(|e| e.to_string())
        });

    if let Err(reason) = result {
        let dialog = AlertDialog::builder()
            .heading("Cancel failed")
            .body(&reason)
            .build();
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.present(window.as_ref());
    }
}

fn confirm_cancel_install(profile_id: &str, window: Option<gtk4::Window>) {
    let dialog = AlertDialog::builder()
        .heading("Cancel download?")
        .body("The current download or runtime setup will stop. You can start it again later.")
        .build();
    dialog.add_response("keep", "Keep downloading");
    dialog.add_response("cancel", "Cancel download");
    dialog.set_response_appearance("cancel", libadwaita::ResponseAppearance::Destructive);
    dialog.set_close_response("keep");

    let profile_id = profile_id.to_string();
    let window_for_response = window.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "cancel" {
            cancel_install(&profile_id, window_for_response.clone());
        }
    });
    dialog.present(window.as_ref());
}

fn clear_list(list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[test]
    fn runtime_name_is_human_readable() {
        assert_eq!(
            runtime_name("ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:rocm"),
            "llm vision whisper runtime (rocm)"
        );
    }

    #[test]
    fn compact_image_ref_keeps_registry_and_image() {
        assert_eq!(
            compact_image_ref("ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:rocm"),
            "ghcr.io/…/aileron-runtime-llm-vision-whisper:rocm"
        );
    }

    #[test]
    fn runtime_setup_text_hides_unknown_size_and_speed() {
        let install = InstallStatus {
            profile_id: "runtime:ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:rocm"
                .to_string(),
            bytes_pulled: 0,
            total_bytes: 0,
            bytes_per_second: 0,
            eta_seconds: -1,
            status: "Pulling runtime image...".to_string(),
            cancel_requested: false,
        };

        let title = runtime_setup_title(&install);
        let detail = runtime_detail_line(&install);

        assert_eq!(title, "Pulling llm vision whisper runtime (rocm)");
        assert_eq!(detail, "ghcr.io/…/aileron-runtime-llm-vision-whisper:rocm");
        assert!(!title.contains("size unknown"));
        assert!(!title.contains("speed calculating"));
        assert!(!detail.contains("size unknown"));
        assert!(!detail.contains("speed calculating"));
    }

    #[test]
    fn profile_subtitle_hides_model_progress_during_runtime_setup() {
        let runtime_install = InstallStatus {
            profile_id: "runtime:ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:vulkan"
                .to_string(),
            bytes_pulled: 0,
            total_bytes: 0,
            bytes_per_second: 0,
            eta_seconds: -1,
            status: "Preparing runtime image...".to_string(),
            cancel_requested: false,
        };
        let subtitle = download_subtitle(
            0,
            600_000_000,
            0,
            -1,
            "Preparing runtime image...",
            false,
            Some(&runtime_install),
        );

        assert_eq!(subtitle, "Preparing llm vision whisper runtime (vulkan)");
        assert!(!subtitle.contains("0.0 / 0.6 GB"));
        assert!(!subtitle.contains("speed calculating"));
    }

    #[test]
    fn profile_subtitle_shows_runtime_percent() {
        let runtime_install = InstallStatus {
            profile_id: "runtime:ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:vulkan"
                .to_string(),
            bytes_pulled: 42,
            total_bytes: 100,
            bytes_per_second: 0,
            eta_seconds: -1,
            status: "Pulling runtime image...".to_string(),
            cancel_requested: false,
        };

        assert_eq!(
            runtime_profile_subtitle(&runtime_install),
            "Pulling llm vision whisper runtime (vulkan) · 42%"
        );
    }

    #[hegel::test]
    fn runtime_percent_is_clamped_to_display_range(tc: TestCase) {
        let bytes_pulled = tc.draw(gs::integers::<i64>().min_value(-200).max_value(200));
        let runtime_install = InstallStatus {
            profile_id: "runtime:ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:vulkan"
                .to_string(),
            bytes_pulled,
            total_bytes: 100,
            bytes_per_second: 0,
            eta_seconds: -1,
            status: "Pulling runtime image...".to_string(),
            cancel_requested: false,
        };

        let expected = (bytes_pulled as f64).clamp(0.0, 100.0);

        assert!(
            runtime_profile_subtitle(&runtime_install).ends_with(&format!(" · {expected:.0}%"))
        );
    }

    #[test]
    fn derives_runtime_id_from_runtime_download_ref() {
        assert_eq!(
            runtime_download_runtime_id(
                "runtime:ghcr.io/razzeee/aileron-runtime-llm-vision-whisper:rocm"
            )
            .as_deref(),
            Some("llm-vision-whisper")
        );
    }

    #[hegel::test]
    fn runtime_download_runtime_id_strips_runtime_prefix_and_tag(tc: TestCase) {
        let runtime_id = tc.draw(gs::sampled_from(vec![
            "llm-vision-whisper".to_string(),
            "stub".to_string(),
        ]));
        let variant = tc.draw(gs::sampled_from(vec![
            "cpu".to_string(),
            "cuda".to_string(),
            "rocm".to_string(),
            "vulkan".to_string(),
        ]));
        let profile_id = format!("runtime:ghcr.io/example/aileron-runtime-{runtime_id}:{variant}");

        assert_eq!(
            runtime_download_runtime_id(&profile_id).as_deref(),
            Some(runtime_id.as_str())
        );
    }

    #[test]
    fn matches_runtime_downloads_to_profile_runtime_ids() {
        let profile_a = install_status("profile-a", "Preparing runtime image...");
        let profile_b = install_status("profile-b", "Preparing runtime image...");
        let downloading_profile = install_status("profile-a", "Downloading model.gguf...");
        let runtime_a = install_status(
            "runtime:ghcr.io/example/aileron-runtime-runtime-a:cpu",
            "Pulling runtime image...",
        );
        let runtime_b = install_status(
            "runtime:ghcr.io/example/aileron-runtime-runtime-b:cpu",
            "Pulling runtime image...",
        );
        let profile_runtime_ids = HashMap::from([
            ("profile-a".to_string(), "runtime-a".to_string()),
            ("profile-b".to_string(), "runtime-b".to_string()),
        ]);

        assert_eq!(
            matching_runtime_installs(
                &profile_a,
                &[runtime_a.clone(), runtime_b.clone()],
                &profile_runtime_ids,
            )
            .iter()
            .map(|install| install.profile_id.as_str())
            .collect::<Vec<_>>(),
            vec![runtime_a.profile_id.as_str()]
        );
        assert_eq!(
            matching_runtime_installs(
                &profile_b,
                &[runtime_a, runtime_b.clone()],
                &profile_runtime_ids
            )
            .iter()
            .map(|install| install.profile_id.as_str())
            .collect::<Vec<_>>(),
            vec![runtime_b.profile_id.as_str()]
        );
        assert!(
            matching_runtime_installs(
                &downloading_profile,
                std::slice::from_ref(&runtime_b),
                &profile_runtime_ids,
            )
            .is_empty()
        );
    }

    fn install_status(profile_id: &str, status: &str) -> InstallStatus {
        InstallStatus {
            profile_id: profile_id.to_string(),
            bytes_pulled: 0,
            total_bytes: 0,
            bytes_per_second: 0,
            eta_seconds: -1,
            status: status.to_string(),
            cancel_requested: false,
        }
    }
}
