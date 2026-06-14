/// OCI runtimes page — list and clean up Aileron-owned Podman runtime images.
use gtk4::prelude::*;
use gtk4::{Button, ListBox, ScrolledWindow};
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
        "Aileron only manages Podman images labeled as Aileron runtimes.",
    ));

    let actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
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
            refresh_runtime_images(&list_box);
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

    use aileron_varlink::aileron_Models::VarlinkClientInterface;
    let conn = match aileron_ipc::client::connect() {
        Ok(conn) => conn,
        Err(e) => {
            append_message(list_box, &format!("Error: {e}"));
            return;
        }
    };

    let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
    match client.list_runtime_images().call() {
        Ok(reply) => {
            if reply.images.is_empty() {
                append_empty_state(list_box);
                return;
            }
            for image in reply.images {
                append_runtime_image_row(list_box, image);
            }
        }
        Err(e) => append_message(list_box, &format!("Error: {e}")),
    }
}

fn append_runtime_image_row(
    list_box: &ListBox,
    image: aileron_varlink::aileron_Models::OciRuntimeImage,
) {
    let row = ActionRow::new();
    let variant = if image.variant.is_empty() {
        "unknown".to_string()
    } else {
        image.variant.clone()
    };
    row.set_title(&format!("{} ({variant})", image.runtime_id));

    let status = if image.in_use {
        format!("in use by {}", image.used_by_profiles.join(", "))
    } else {
        "unused".to_string()
    };
    row.set_subtitle(&format!(
        "{} · {} · {status}",
        image.image_ref,
        format_bytes(image.size_bytes),
    ));

    if !image.in_use {
        let remove_button = Button::with_label("Remove");
        remove_button.add_css_class("destructive-action");
        let list_box = list_box.clone();
        let image_id = image.image_id.clone();
        remove_button.connect_clicked(move |_| {
            remove_runtime_image(&list_box, &image_id);
            refresh_runtime_images(&list_box);
        });
        row.add_suffix(&remove_button);
    }

    list_box.append(&row);
}

fn prune_unused_runtime_images(list_box: &ListBox) {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;
    let result = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client
                .prune_unused_runtime_images()
                .call()
                .map_err(|e| e.to_string())
        });
    if let Err(e) = result {
        append_message(list_box, &format!("Cleanup failed: {e}"));
    }
}

fn remove_runtime_image(list_box: &ListBox, image_id: &str) {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;
    let result = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client
                .remove_runtime_image(image_id.to_string())
                .call()
                .map_err(|e| e.to_string())
        });
    if let Err(e) = result {
        append_message(list_box, &format!("Remove failed: {e}"));
    }
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

    #[test]
    fn formats_runtime_image_sizes() {
        assert_eq!(format_bytes(0), "unknown size");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
    }
}
