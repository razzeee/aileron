/// OCI runtimes page — list and clean up Aileron-owned OCI runtime images.
use std::collections::HashMap;

use aileron_varlink::aileron_Models::{InstallStatus, OciRuntimeImage};
use gtk4::prelude::*;
use gtk4::{Box, Button, Label, ListBox, Orientation, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, AlertDialog, PreferencesGroup, PreferencesPage};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use super::{install_is_terminal_status, source_label};

pub struct RuntimesPage;

#[derive(Debug)]
pub enum RuntimesMsg {
    Refresh,
}

pub struct RuntimesWidgets {
    list_box: ListBox,
}

impl SimpleComponent for RuntimesPage {
    type Init = ();
    type Input = RuntimesMsg;
    type Output = ();
    type Widgets = RuntimesWidgets;
    type Root = PreferencesPage;

    fn init_root() -> Self::Root {
        PreferencesPage::new()
    }

    fn init(
        (): Self::Init,
        page: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let list_box = build_page(&page);
        refresh_runtime_images(&list_box);
        ComponentParts {
            model: RuntimesPage,
            widgets: RuntimesWidgets { list_box },
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            RuntimesMsg::Refresh => {}
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        refresh_runtime_images(&widgets.list_box);
    }
}

fn build_page(page: &PreferencesPage) -> ListBox {
    let group = PreferencesGroup::new();
    group.set_title("OCI runtime images");
    group.set_description(Some(
        "Aileron only manages OCI images labeled as Aileron runtimes.",
    ));

    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    actions.set_halign(gtk4::Align::End);
    actions.set_valign(gtk4::Align::Center);
    let prune_button = Button::with_label("Remove unused");
    prune_button.add_css_class("destructive-action");
    prune_button.set_valign(gtk4::Align::Center);
    prune_button.set_tooltip_text(Some(
        "Remove Aileron runtime images that are not used by any installed profile.",
    ));
    actions.append(&prune_button);
    group.set_header_suffix(Some(&actions));

    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(300)
        .child(&list_box)
        .build();
    group.add(&scroll);

    {
        let list_box = list_box.clone();
        prune_button.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            confirm_prune_unused_runtime_images(&list_box, window.as_ref());
        });
    }

    refresh_runtime_images(&list_box);
    page.add(&group);
    list_box
}

fn refresh_runtime_images(list_box: &ListBox) {
    clear_list(list_box);
    append_message(list_box, "Loading runtime images");

    let list_box = list_box.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            use aileron_varlink::aileron_Models::VarlinkClientInterface;

            aileron_ipc::client::connect()
                .map_err(|e| e.to_string())
                .and_then(|conn| {
                    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                    let images = client
                        .list_runtime_images()
                        .call()
                        .map(|reply| reply.images)
                        .map_err(|e| e.to_string())?;
                    let installs = client
                        .list_installs()
                        .call()
                        .map(|reply| reply.installs)
                        .map_err(|e| e.to_string())?;
                    Ok((images, installs))
                })
        })
        .await
        .map_err(|_| "Runtime image list task failed".to_string())
        .and_then(|result| result);

        render_runtime_images(&list_box, result);
    });
}

fn render_runtime_images(
    list_box: &ListBox,
    result: Result<(Vec<OciRuntimeImage>, Vec<InstallStatus>), String>,
) {
    clear_list(list_box);
    match result {
        Ok((images, installs)) => {
            if images.is_empty() {
                append_empty_state(list_box);
                return;
            }
            let has_pending_update_check = images
                .iter()
                .any(|image| image.update_status == "checking for updates");
            let active_runtime_downloads = installs
                .into_iter()
                .filter(is_active_runtime_download)
                .map(|install| {
                    (
                        runtime_download_image_ref(&install.profile_id).to_string(),
                        install,
                    )
                })
                .collect::<HashMap<_, _>>();
            for image in images {
                let active_download = active_runtime_downloads.get(&image.image_ref);
                append_runtime_image_row(list_box, image, active_download);
            }
            if runtime_list_should_refresh(
                has_pending_update_check,
                !active_runtime_downloads.is_empty(),
            ) {
                refresh_runtime_images_after_pending_work(list_box);
            }
        }
        Err(e) => append_message(list_box, &format!("Error: {e}")),
    }
}

fn refresh_runtime_images_after_pending_work(list_box: &ListBox) {
    let list_box = list_box.clone();
    glib::timeout_add_seconds_local(2, move || {
        refresh_runtime_images(&list_box);
        glib::ControlFlow::Break
    });
}

fn runtime_list_should_refresh(has_pending_update_check: bool, has_active_download: bool) -> bool {
    has_pending_update_check || has_active_download
}

fn append_runtime_image_row(
    list_box: &ListBox,
    image: OciRuntimeImage,
    active_download: Option<&InstallStatus>,
) {
    let row = Box::new(Orientation::Horizontal, 18);
    row.set_margin_top(12);
    row.set_margin_bottom(12);
    row.set_margin_start(14);
    row.set_margin_end(14);

    let details = Box::new(Orientation::Vertical, 5);
    details.set_hexpand(true);

    let variant = if image.variant.is_empty() {
        "unknown".to_string()
    } else {
        image.variant.clone()
    };

    let title = Label::new(Some(&format!("{} ({variant})", image.runtime_id)));
    title.set_xalign(0.0);
    title.add_css_class("heading");
    details.append(&title);

    let image_ref = Label::new(Some(&image.image_ref));
    image_ref.set_xalign(0.0);
    image_ref.set_wrap(true);
    image_ref.set_wrap_mode(gtk4::pango::WrapMode::Char);
    image_ref.add_css_class("dim-label");
    details.append(&image_ref);

    let usage = if image.in_use {
        format!("in use by {}", image.used_by_profiles.join(", "))
    } else {
        "unused".to_string()
    };
    let status = if let Some(download) = active_download {
        format!("updating · {}", download.status)
    } else if image.update_status.is_empty() {
        "status unknown".to_string()
    } else {
        runtime_status_label(&image.update_status).to_string()
    };

    let metadata = Label::new(Some(&format!(
        "{} · {} · {usage} · {status}",
        format_bytes(image.size_bytes),
        source_label(&image.source),
    )));
    metadata.set_xalign(0.0);
    metadata.set_wrap(true);
    metadata.add_css_class("dim-label");
    details.append(&metadata);

    row.append(&details);

    let actions = Box::new(Orientation::Horizontal, 8);
    actions.set_valign(gtk4::Align::Center);
    actions.set_halign(gtk4::Align::End);

    if runtime_update_action_visible(&image, active_download.is_some()) {
        let update_button = Button::with_label(if active_download.is_some() {
            "Updating"
        } else {
            "Update"
        });
        update_button.add_css_class("suggested-action");
        update_button.set_sensitive(active_download.is_none());
        let list_box = list_box.clone();
        let image_ref = image.image_ref.clone();
        update_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            button.set_label("Updating");
            update_runtime_image(&list_box, &image_ref);
        });
        actions.append(&update_button);
    }

    if !image.in_use && image.source != "system" {
        let remove_button = Button::with_label("Remove");
        remove_button.add_css_class("destructive-action");
        remove_button.set_sensitive(active_download.is_none());
        let list_box = list_box.clone();
        let image_id = image.image_id.clone();
        remove_button.connect_clicked(move |btn| {
            let window = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok());
            confirm_remove_runtime_image(&list_box, &image_id, window.as_ref());
        });
        actions.append(&remove_button);
    }

    if actions.first_child().is_some() {
        row.append(&actions);
    }

    list_box.append(&row);
}

fn is_active_runtime_download(install: &InstallStatus) -> bool {
    install.profile_id.starts_with("runtime:") && !install_is_terminal(install)
}

fn runtime_update_action_visible(image: &OciRuntimeImage, has_active_download: bool) -> bool {
    image.source != "system" && (has_active_download || image.update_available)
}

fn runtime_status_label(status: &str) -> &str {
    if status == "installed: update not checked" {
        "installed"
    } else {
        status
    }
}

fn install_is_terminal(install: &InstallStatus) -> bool {
    install_is_terminal_status(&install.status)
}

fn runtime_download_image_ref(profile_id: &str) -> &str {
    profile_id.strip_prefix("runtime:").unwrap_or(profile_id)
}

fn update_runtime_image(list_box: &ListBox, image_ref: &str) {
    clear_list(list_box);
    append_message(list_box, "Updating runtime image");

    let list_box = list_box.clone();
    let image_ref = image_ref.to_string();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            use aileron_varlink::aileron_Models::VarlinkClientInterface;

            aileron_ipc::client::connect()
                .map_err(|e| e.to_string())
                .and_then(|conn| {
                    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                    client
                        .update_runtime_image(image_ref)
                        .call()
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
        })
        .await
        .map_err(|_| "Runtime image update task failed".to_string())
        .and_then(|result| result);

        match result {
            Ok(()) => refresh_runtime_images(&list_box),
            Err(e) => {
                clear_list(&list_box);
                append_message(&list_box, &format!("Update failed: {e}"));
            }
        }
    });
}

fn prune_unused_runtime_images(list_box: &ListBox) {
    clear_list(list_box);
    append_message(list_box, "Removing unused runtime images");

    let list_box = list_box.clone();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            use aileron_varlink::aileron_Models::VarlinkClientInterface;

            aileron_ipc::client::connect()
                .map_err(|e| e.to_string())
                .and_then(|conn| {
                    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                    client
                        .prune_unused_runtime_images()
                        .call()
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
        })
        .await
        .map_err(|_| "Runtime image cleanup task failed".to_string())
        .and_then(|result| result);

        match result {
            Ok(()) => refresh_runtime_images(&list_box),
            Err(e) => {
                clear_list(&list_box);
                append_message(&list_box, &format!("Cleanup failed: {e}"));
            }
        }
    });
}

fn confirm_prune_unused_runtime_images(list_box: &ListBox, window: Option<&gtk4::Window>) {
    let dialog = AlertDialog::builder()
        .heading("Remove unused runtime images?")
        .body("This removes Aileron runtime images that are not used by any installed profile.")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", libadwaita::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");

    let list_box = list_box.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "remove" {
            prune_unused_runtime_images(&list_box);
        }
    });
    dialog.present(window);
}

fn remove_runtime_image(list_box: &ListBox, image_id: &str) {
    clear_list(list_box);
    append_message(list_box, "Removing runtime image");

    let list_box = list_box.clone();
    let image_id = image_id.to_string();
    glib::spawn_future_local(async move {
        let result = gio::spawn_blocking(move || {
            use aileron_varlink::aileron_Models::VarlinkClientInterface;

            aileron_ipc::client::connect()
                .map_err(|e| e.to_string())
                .and_then(|conn| {
                    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                    client
                        .remove_runtime_image(image_id)
                        .call()
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                })
        })
        .await
        .map_err(|_| "Runtime image remove task failed".to_string())
        .and_then(|result| result);

        match result {
            Ok(()) => refresh_runtime_images(&list_box),
            Err(e) => {
                clear_list(&list_box);
                append_message(&list_box, &format!("Remove failed: {e}"));
            }
        }
    });
}

fn confirm_remove_runtime_image(list_box: &ListBox, image_id: &str, window: Option<&gtk4::Window>) {
    let dialog = AlertDialog::builder()
        .heading("Remove runtime image?")
        .body("This removes the selected runtime image from local storage.")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_response_appearance("remove", libadwaita::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");

    let list_box = list_box.clone();
    let image_id = image_id.to_string();
    dialog.connect_response(None, move |_, response| {
        if response == "remove" {
            remove_runtime_image(&list_box, &image_id);
        }
    });
    dialog.present(window);
}

fn append_message(list_box: &ListBox, message: &str) {
    let row = ActionRow::new();
    row.set_title(message);
    list_box.append(&row);
}

fn append_empty_state(list_box: &ListBox) {
    let row = ActionRow::new();
    row.set_title("No runtime images installed");
    row.set_subtitle("Runtime images appear here after Aileron pulls a labeled OCI image.");
    list_box.append(&row);
}

fn clear_list(list_box: &ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
}

fn format_bytes(bytes: i64) -> String {
    if bytes <= 0 {
        return "unknown size".to_string();
    }
    let bytes = bytes as f64;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes;
    let mut unit = units[0];
    for next_unit in units.iter().skip(1) {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next_unit;
    }
    if unit == "B" {
        format!("{} {unit}", value as i64)
    } else {
        format!("{value:.1} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::TestCase;
    use hegel::generators as gs;

    #[hegel::test]
    fn formats_non_positive_runtime_image_sizes_as_unknown(tc: TestCase) {
        let bytes = tc.draw(gs::integers::<i64>().max_value(0));

        assert_eq!(format_bytes(bytes), "unknown size");
    }

    #[hegel::test]
    fn formats_runtime_image_sizes_with_expected_units(tc: TestCase) {
        let bytes = tc.draw(gs::integers::<i64>().min_value(1).max_value(i64::MAX / 2));
        let formatted = format_bytes(bytes);

        assert!(
            [" B", " KB", " MB", " GB", " TB"]
                .iter()
                .any(|unit| formatted.ends_with(unit)),
            "unexpected formatted size: {formatted}"
        );
        assert!(!formatted.contains("unknown"));
    }

    #[test]
    fn formats_runtime_image_size_boundaries() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn hides_update_action_for_unchecked_runtime_status() {
        let image = OciRuntimeImage {
            image_id: "runtime".to_string(),
            image_ref: "ghcr.io/example/runtime:cpu".to_string(),
            runtime_id: "llm-vision-whisper".to_string(),
            variant: "cpu".to_string(),
            size_bytes: 1,
            in_use: true,
            used_by_profiles: vec!["profile".to_string()],
            update_available: false,
            update_status: "installed: update not checked".to_string(),
            source: "user".to_string(),
        };

        assert!(!runtime_update_action_visible(&image, false));
        assert!(runtime_update_action_visible(&image, true));
    }

    #[test]
    fn shows_update_action_when_update_is_available() {
        let image = OciRuntimeImage {
            image_id: "runtime".to_string(),
            image_ref: "ghcr.io/example/runtime:cpu".to_string(),
            runtime_id: "llm-vision-whisper".to_string(),
            variant: "cpu".to_string(),
            size_bytes: 1,
            in_use: true,
            used_by_profiles: vec!["profile".to_string()],
            update_available: true,
            update_status: "update available".to_string(),
            source: "user".to_string(),
        };

        assert!(runtime_update_action_visible(&image, false));
    }

    #[test]
    fn refreshes_while_runtime_work_is_pending() {
        assert!(runtime_list_should_refresh(true, false));
        assert!(runtime_list_should_refresh(false, true));
        assert!(!runtime_list_should_refresh(false, false));
    }

    #[test]
    fn runtime_status_label_hides_unchecked_update_wording() {
        assert_eq!(
            runtime_status_label("installed: update not checked"),
            "installed"
        );
        assert_eq!(runtime_status_label("not checkable"), "not checkable");
    }
}
