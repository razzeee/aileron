/// Models page — list pulled OCI images, pull new images, assign use-cases, delete.
use gtk4::prelude::*;
use gtk4::{Box, Button, Entry, ListBox, Orientation, ProgressBar, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, PreferencesGroup, PreferencesPage};

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

    {
        let entry = image_entry.clone();
        let progress = progress.clone();
        pull_button.connect_clicked(move |_| {
            let image_ref = entry.text().to_string();
            if image_ref.is_empty() {
                return;
            }
            progress.set_visible(true);
            progress.pulse();

            let progress_clone = progress.clone();
            glib::spawn_future_local(async move {
                let _ = gio::spawn_blocking(move || {
                    use aileron_varlink::aileron_Models::VarlinkClientInterface;
                    if let Ok(conn) = aileron_ipc::client::connect() {
                        let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
                        let _ = client.pull(image_ref).call();
                    }
                })
                .await;
                progress_clone.set_fraction(1.0);
            });
        });
    }

    page.add(&pull_group);

    // ── Installed models group ────────────────────────────────────────────────
    let models_group = PreferencesGroup::new();
    models_group.set_title("Installed Models");

    let refresh_button = Button::with_label("Refresh");
    models_group.set_header_suffix(Some(&refresh_button));

    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

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
