/// OCI runtimes page — list and clean up Aileron-owned OCI runtime images.
use std::collections::HashMap;

use aileron_varlink::aileron_Models::{InstallStatus, OciRuntimeImage};
use gtk4::prelude::*;
use gtk4::{Box, Button, Label, ListBox, Orientation, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, PreferencesGroup, PreferencesPage};

#[derive(Clone)]
pub struct RuntimeImagesView {
    pub widget: gtk4::Widget,
    list_box: ListBox,
}

impl RuntimeImagesView {
    pub fn refresh(&self) {
        refresh_runtime_images(&self.list_box);
    }
}

pub fn build() -> RuntimeImagesView {
    let page = PreferencesPage::new();

    let group = PreferencesGroup::new();
    group.set_title("OCI Runtime Images");
    group.set_description(Some(
        "Aileron only manages OCI images labeled as Aileron runtimes.",
    ));

    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    actions.set_halign(gtk4::Align::End);
    actions.set_valign(gtk4::Align::Center);
    let prune_button = Button::with_label("Remove Unused");
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
        prune_button.connect_clicked(move |_| {
            prune_unused_runtime_images(&list_box);
        });
    }

    refresh_runtime_images(&list_box);
    page.add(&group);
    RuntimeImagesView {
        widget: page.upcast(),
        list_box,
    }
}

fn refresh_runtime_images(list_box: &ListBox) {
    clear_list(list_box);
    append_message(list_box, "Loading runtime images...");

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
        }
        Err(e) => append_message(list_box, &format!("Error: {e}")),
    }
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
        image.update_status.clone()
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

    if image.source != "system" && (image.update_available || active_download.is_some()) {
        let update_button = Button::with_label(if active_download.is_some() {
            "Updating..."
        } else {
            "Update"
        });
        update_button.add_css_class("suggested-action");
        update_button.set_sensitive(active_download.is_none());
        let list_box = list_box.clone();
        let image_ref = image.image_ref.clone();
        update_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            button.set_label("Updating...");
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
        remove_button.connect_clicked(move |_| {
            remove_runtime_image(&list_box, &image_id);
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

fn install_is_terminal(install: &InstallStatus) -> bool {
    install.status.starts_with("Failed:") || install.status == "Completed"
}

fn runtime_download_image_ref(profile_id: &str) -> &str {
    profile_id.strip_prefix("runtime:").unwrap_or(profile_id)
}

fn update_runtime_image(list_box: &ListBox, image_ref: &str) {
    clear_list(list_box);
    append_message(list_box, "Updating runtime image...");

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
    append_message(list_box, "Removing unused runtime images...");

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

fn remove_runtime_image(list_box: &ListBox, image_id: &str) {
    clear_list(list_box);
    append_message(list_box, "Removing runtime image...");

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

fn source_label(source: &str) -> &'static str {
    match source {
        "system" => "System",
        "user" => "User",
        _ => "Unknown source",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_runtime_image_sizes() {
        assert_eq!(format_bytes(0), "unknown size");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    }
}
