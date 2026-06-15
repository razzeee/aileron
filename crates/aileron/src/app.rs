use std::rc::Rc;

use libadwaita::prelude::*;
use libadwaita::{ApplicationWindow, HeaderBar, ToolbarView, ViewStack, ViewSwitcher};
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmApp,
    SimpleComponent,
};

use crate::pages::{activity_page, downloads_page, models_page, permissions_page, runtimes_page};

#[derive(Debug)]
pub enum AppMsg {}

pub struct AppModel {
    _permissions: Controller<permissions_page::PermissionsPage>,
    _downloads: Controller<downloads_page::DownloadsPage>,
    _activity: Controller<activity_page::ActivityPage>,
}

pub struct AppWidgets;

pub fn run() {
    libadwaita::init().expect("failed to initialise libadwaita");
    let app = RelmApp::new("org.aileron.Manager");
    app.run::<AppModel>(());
}

impl SimpleComponent for AppModel {
    type Init = ();
    type Input = AppMsg;
    type Output = ();
    type Widgets = AppWidgets;
    type Root = ApplicationWindow;

    fn init_root() -> Self::Root {
        ApplicationWindow::builder()
            .title("Aileron")
            .default_width(860)
            .default_height(640)
            .build()
    }

    fn init(
        (): Self::Init,
        window: Self::Root,
        _sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let permissions = permissions_page::PermissionsPage::builder()
            .launch(())
            .detach();
        let downloads = downloads_page::DownloadsPage::builder().launch(()).detach();
        let activity = activity_page::ActivityPage::builder().launch(()).detach();

        build_window(&window, &permissions, &downloads, &activity);
        ComponentParts {
            model: AppModel {
                _permissions: permissions,
                _downloads: downloads,
                _activity: activity,
            },
            widgets: AppWidgets,
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {}
    }
}

fn build_window(
    window: &ApplicationWindow,
    permissions: &Controller<permissions_page::PermissionsPage>,
    downloads: &Controller<downloads_page::DownloadsPage>,
    activity: &Controller<activity_page::ActivityPage>,
) {
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

    let perms_page = stack.add_titled(permissions.widget(), Some("permissions"), "Permissions");
    perms_page.set_icon_name(Some("system-lock-screen-symbolic"));

    let downloads_page = stack.add_titled(downloads.widget(), Some("downloads"), "Downloads");
    downloads_page.set_icon_name(Some("emblem-downloads-symbolic"));

    let runtimes_page = stack.add_titled(&runtimes_view.widget, Some("runtimes"), "Runtimes");
    runtimes_page.set_icon_name(Some("package-x-generic-symbolic"));

    let activity_page = stack.add_titled(activity.widget(), Some("activity"), "Activity");
    activity_page.set_icon_name(Some("emblem-synchronizing-symbolic"));

    let downloads_sender = downloads.sender().clone();
    stack.connect_visible_child_name_notify(move |stack| {
        match stack.visible_child_name().as_deref() {
            Some("runtimes") => refresh_runtimes(),
            Some("downloads") => {
                let _ = downloads_sender.send(downloads_page::DownloadsMsg::Refresh);
            }
            _ => {}
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

    window.set_content(Some(&toolbar_view));
}
