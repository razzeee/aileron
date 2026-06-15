/// Activity page — live list of active sessions, polled every 2 seconds.
use gtk4::prelude::*;
use gtk4::{Button, ListBox, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, PreferencesGroup, PreferencesPage};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

pub struct ActivityPage;

#[derive(Debug)]
pub enum ActivityMsg {
    Refresh,
}

pub struct ActivityWidgets {
    list_box: ListBox,
}

impl SimpleComponent for ActivityPage {
    type Init = ();
    type Input = ActivityMsg;
    type Output = ();
    type Widgets = ActivityWidgets;
    type Root = PreferencesPage;

    fn init_root() -> Self::Root {
        PreferencesPage::new()
    }

    fn init(
        (): Self::Init,
        page: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let list_box = build_page(&page, sender.clone());
        refresh_sessions(&list_box);
        ComponentParts {
            model: ActivityPage,
            widgets: ActivityWidgets { list_box },
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            ActivityMsg::Refresh => {}
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        refresh_sessions(&widgets.list_box);
    }
}

fn build_page(page: &PreferencesPage, sender: ComponentSender<ActivityPage>) -> ListBox {
    let group = PreferencesGroup::new();
    group.set_title("Active Sessions");
    group.set_description(Some("Refreshes automatically every 2 seconds."));

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
        glib::timeout_add_seconds_local(2, move || {
            sender.input(ActivityMsg::Refresh);
            glib::ControlFlow::Continue
        });
    }

    page.add(&group);
    list_box
}

fn refresh_sessions(list_box: &ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    use aileron_varlink::aileron_Sessions::VarlinkClientInterface;
    let conn = match aileron_ipc::client::connect() {
        Ok(_c) => _c,
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
                return;
            }
            for session in &reply.sessions {
                let row = ActionRow::new();
                row.set_title(&format!("{} — {}", session.app_id, session.use_case));
                row.set_subtitle(&format!("started: {}", session.started_at));

                let kill_btn = Button::with_label("Kill");
                kill_btn.add_css_class("destructive-action");
                kill_btn.set_valign(gtk4::Align::Center);
                let session_id = session.session_id.clone();
                let list_box_ref = list_box.clone();
                kill_btn.connect_clicked(move |_| {
                    use aileron_varlink::aileron_Sessions::VarlinkClientInterface;
                    if let Ok(conn) = aileron_ipc::client::connect() {
                        let mut c = aileron_varlink::aileron_Sessions::VarlinkClient::new(conn);
                        let _ = c.kill_session(session_id.clone()).call();
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
