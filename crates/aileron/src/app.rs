use gtk4::prelude::*;
use libadwaita::prelude::*;
use libadwaita::{Application, ApplicationWindow, HeaderBar, NavigationView, NavigationPage};

use crate::pages::{models_page, permissions_page, activity_page};

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
    let nav_view = NavigationView::new();

    // Models page
    let models_nav = NavigationPage::builder()
        .title("Models")
        .tag("models")
        .child(&models_page::build())
        .build();

    // Permissions page
    let perms_nav = NavigationPage::builder()
        .title("Permissions")
        .tag("permissions")
        .child(&permissions_page::build())
        .build();

    // Activity page
    let activity_nav = NavigationPage::builder()
        .title("Activity")
        .tag("activity")
        .child(&activity_page::build())
        .build();

    nav_view.add(&models_nav);

    // Sidebar switcher row
    let toolbar_view = libadwaita::ToolbarView::new();
    let header = HeaderBar::new();

    // Navigation switcher buttons in the header.
    let switcher = libadwaita::ViewSwitcher::new();
    let stack = gtk4::Stack::new();

    stack.add_named(&models_page::build(), Some("models"));
    stack.add_named(&permissions_page::build(), Some("permissions"));
    stack.add_named(&activity_page::build(), Some("activity"));

    stack.child_by_name("models")
        .unwrap()
        .set_property("title", "Models");
    stack.child_by_name("permissions")
        .unwrap()
        .set_property("title", "Permissions");
    stack.child_by_name("activity")
        .unwrap()
        .set_property("title", "Activity");

    switcher.set_stack(Some(&stack));
    header.set_title_widget(Some(&switcher));

    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&stack));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Aileron")
        .default_width(800)
        .default_height(600)
        .content(&toolbar_view)
        .build();

    window.present();
}
