/// Permissions page — per-app, per-use-case toggles with last-used timestamps.

use gtk4::prelude::*;
use libadwaita::prelude::*;
use gtk4::{Box, Button, ListBox, Orientation, ScrolledWindow};
use libadwaita::{ActionRow, PreferencesGroup, SwitchRow};

pub fn build() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let group = PreferencesGroup::new();
    group.set_title("App Permissions");

    let refresh_button = Button::with_label("Refresh");
    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);

    {
        let list_box = list_box.clone();
        refresh_button.connect_clicked(move |_| {
            refresh_permissions(&list_box);
        });
    }

    refresh_permissions(&list_box);

    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&list_box)
        .build();

    group.add(&refresh_button);
    group.add(&scroll);
    vbox.append(&group);
    vbox.upcast()
}

fn refresh_permissions(list_box: &ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    use aileron_varlink::aileron_Permissions::VarlinkClientInterface;

    let conn = match aileron_ipc::client::connect() {
        Ok(c) => c,
        Err(e) => {
            let row = ActionRow::new();
            row.set_title(&format!("Error: {e}"));
            list_box.append(&row);
            return;
        }
    };

    let mut client = aileron_varlink::aileron_Permissions::VarlinkClient::new(conn);
    match client.list_app_permissions().call() {
        Ok(reply) => {
            if reply.permissions.is_empty() {
                let row = ActionRow::new();
                row.set_title("No permissions recorded");
                list_box.append(&row);
            }
            for perm in &reply.permissions {
                let row = SwitchRow::builder()
                    .title(&perm.use_case)
                    .active(perm.allowed)
                    .build();

                let subtitle = match &perm.last_used {
                    Some(lu) => format!("{} — last used: {}", perm.app_id, lu),
                    None => perm.app_id.clone(),
                };
                row.set_subtitle(&subtitle);

                let app_id = perm.app_id.clone();
                let use_case = perm.use_case.clone();
                row.connect_active_notify(move |switch| {
                    use aileron_varlink::aileron_Permissions::VarlinkClientInterface;
                    let allowed = switch.is_active();
                    if let Ok(conn) = aileron_ipc::client::connect() {
                        let mut client =
                            aileron_varlink::aileron_Permissions::VarlinkClient::new(conn);
                        let _ = client
                            .set_app_permission(
                                app_id.clone(),
                                use_case.clone(),
                                allowed,
                            )
                            .call();
                    }
                });

                list_box.append(&row);
            }
        }
        Err(e) => {
            let row = ActionRow::new();
            row.set_title(&format!("Error: {e}"));
            list_box.append(&row);
        }
    }
}
