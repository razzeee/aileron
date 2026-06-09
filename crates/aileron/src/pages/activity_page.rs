/// Activity page — live list of active sessions, polled every 2 seconds.

use gtk4::prelude::*;
use libadwaita::prelude::*;
use gtk4::{Box, Button, ListBox, Orientation, ScrolledWindow};
use libadwaita::{ActionRow, PreferencesGroup};

pub fn build() -> gtk4::Widget {
    let vbox = Box::new(Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let group = PreferencesGroup::new();
    group.set_title("Active Sessions");

    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);

    refresh_sessions(&list_box);

    // Auto-refresh every 2 seconds.
    {
        let list_box = list_box.clone();
        glib::timeout_add_seconds_local(2, move || {
            refresh_sessions(&list_box);
            glib::ControlFlow::Continue
        });
    }

    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&list_box)
        .build();

    group.add(&scroll);
    vbox.append(&group);
    vbox.upcast()
}

fn refresh_sessions(list_box: &ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    use aileron_varlink::aileron_Sessions::VarlinkClientInterface;

    let conn = match aileron_ipc::client::connect() {
        Ok(c) => c,
        Err(_) => {
            let row = ActionRow::new();
            row.set_title("Daemon not reachable");
            list_box.append(&row);
            return;
        }
    };

    let mut client = aileron_varlink::aileron_Sessions::VarlinkClient::new(conn);
    match client.list_active().call() {
        Ok(reply) => {
            if reply.sessions.is_empty() {
                let row = ActionRow::new();
                row.set_title("No active sessions");
                list_box.append(&row);
            }
            for session in &reply.sessions {
                let row = ActionRow::new();
                row.set_title(&format!("{} — {}", session.app_id, session.use_case));
                row.set_subtitle(&format!("started: {}", session.started_at));

                let kill_btn = Button::with_label("Kill");
                kill_btn.add_css_class("destructive-action");
                let session_id = session.session_id.clone();
                let list_box_ref = list_box.clone();
                kill_btn.connect_clicked(move |_| {
                    use aileron_varlink::aileron_Sessions::VarlinkClientInterface;
                    if let Ok(conn) = aileron_ipc::client::connect() {
                        let mut client =
                            aileron_varlink::aileron_Sessions::VarlinkClient::new(conn);
                        let _ = client.kill_session(session_id.clone()).call();
                    }
                    refresh_sessions(&list_box_ref);
                });
                row.add_suffix(&kill_btn);

                list_box.append(&row);
            }
        }
        Err(_) => {
            let row = ActionRow::new();
            row.set_title("No active sessions");
            list_box.append(&row);
        }
    }
}
