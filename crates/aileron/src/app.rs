use std::rc::Rc;

use gtk4::prelude::*;
use libadwaita::{Application, ApplicationWindow, HeaderBar, ToolbarView, ViewStack, ViewSwitcher};

use crate::pages::{activity_page, models_page, permissions_page, runtimes_page};

pub fn build_app() -> Application {
    let app = Application::builder()
        .application_id("org.aileron.Manager")
        .build();

    app.connect_activate(|app| {
        build_window(app);
    });

    app
}

fn build_window(app: &Application) {
    // AdwViewStack provides the per-page title/icon metadata that AdwViewSwitcher needs.
    let stack = ViewStack::new();

    let runtimes_view = runtimes_page::build();
    let refresh_runtimes = {
        let runtimes_view = runtimes_view.clone();
        Rc::new(move || runtimes_view.refresh())
    };

    let models_page = stack.add_titled(
        &models_page::build(refresh_runtimes.clone()),
        Some("profiles"),
        "Profiles",
    );
    models_page.set_icon_name(Some("drive-harddisk-symbolic"));

    let perms_page = stack.add_titled(
        &permissions_page::build(),
        Some("permissions"),
        "Permissions",
    );
    perms_page.set_icon_name(Some("system-lock-screen-symbolic"));

    let runtimes_page = stack.add_titled(&runtimes_view.widget, Some("runtimes"), "Runtimes");
    runtimes_page.set_icon_name(Some("package-x-generic-symbolic"));

    let activity_page = stack.add_titled(&activity_page::build(), Some("activity"), "Activity");
    activity_page.set_icon_name(Some("emblem-synchronizing-symbolic"));

    stack.connect_visible_child_name_notify(move |stack| {
        if stack.visible_child_name().as_deref() == Some("runtimes") {
            refresh_runtimes();
        }
    });

    // The switcher sits in the header bar.
    let switcher = ViewSwitcher::builder()
        .stack(&stack)
        .policy(libadwaita::ViewSwitcherPolicy::Wide)
        .build();

    let header = HeaderBar::new();
    header.set_title_widget(Some(&switcher));

    let toolbar_view = ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&stack));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aileron")
        .default_width(860)
        .default_height(640)
        .content(&toolbar_view)
        .build();

    window.present();
}
