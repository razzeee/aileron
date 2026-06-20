use libadwaita::prelude::*;
use libadwaita::{
    ApplicationWindow, HeaderBar, OverlaySplitView, ToolbarView, ViewStack, ViewSwitcherSidebar,
    WindowTitle,
};
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmApp,
    SimpleComponent,
};

use crate::pages::{
    activity_page, downloads_page, models_page, overview_page, permissions_page, runtimes_page,
};

#[derive(Debug)]
pub enum AppMsg {}

pub struct AppModel {
    _overview: Controller<overview_page::OverviewPage>,
    _models: Controller<models_page::ModelsPage>,
    _runtimes: Controller<runtimes_page::RuntimesPage>,
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
        let overview = overview_page::OverviewPage::builder().launch(()).detach();
        let runtimes = runtimes_page::RuntimesPage::builder().launch(()).detach();
        let runtimes_sender = runtimes.sender().clone();
        let models = models_page::ModelsPage::builder()
            .launch(std::rc::Rc::new(move || {
                let _ = runtimes_sender.send(runtimes_page::RuntimesMsg::Refresh);
            }))
            .detach();
        let permissions = permissions_page::PermissionsPage::builder()
            .launch(())
            .detach();
        let downloads = downloads_page::DownloadsPage::builder().launch(()).detach();
        let activity = activity_page::ActivityPage::builder().launch(()).detach();

        build_window(
            &window,
            &overview,
            &models,
            &runtimes,
            &permissions,
            &downloads,
            &activity,
        );
        ComponentParts {
            model: AppModel {
                _overview: overview,
                _models: models,
                _runtimes: runtimes,
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
    overview: &Controller<overview_page::OverviewPage>,
    models: &Controller<models_page::ModelsPage>,
    runtimes: &Controller<runtimes_page::RuntimesPage>,
    permissions: &Controller<permissions_page::PermissionsPage>,
    downloads: &Controller<downloads_page::DownloadsPage>,
    activity: &Controller<activity_page::ActivityPage>,
) {
    // AdwViewStack provides the per-page title/icon metadata that AdwViewSwitcher needs.
    let stack = ViewStack::new();

    let overview_page = stack.add_titled(overview.widget(), Some("overview"), "Overview");
    overview_page.set_icon_name(Some("view-grid-symbolic"));

    let models_page = stack.add_titled(models.widget(), Some("profiles"), "Profiles");
    models_page.set_icon_name(Some("preferences-system-symbolic"));

    let runtimes_page = stack.add_titled(runtimes.widget(), Some("runtimes"), "Runtimes");
    runtimes_page.set_icon_name(Some("applications-system-symbolic"));

    let perms_page = stack.add_titled(permissions.widget(), Some("permissions"), "Permissions");
    perms_page.set_icon_name(Some("system-lock-screen-symbolic"));

    let downloads_page = stack.add_titled(downloads.widget(), Some("downloads"), "Downloads");
    downloads_page.set_icon_name(Some("folder-download-symbolic"));

    let activity_page = stack.add_titled(activity.widget(), Some("activity"), "Activity");
    activity_page.set_icon_name(Some("media-playlist-repeat-symbolic"));
    stack.set_visible_child_name("overview");

    let downloads_sender = downloads.sender().clone();
    let runtimes_sender = runtimes.sender().clone();
    let overview_sender = overview.sender().clone();
    stack.connect_visible_child_name_notify(move |stack| {
        match stack.visible_child_name().as_deref() {
            Some("overview") => {
                let _ = overview_sender.send(overview_page::OverviewMsg::Refresh);
            }
            Some("runtimes") => {
                let _ = runtimes_sender.send(runtimes_page::RuntimesMsg::Refresh);
            }
            Some("downloads") => {
                let _ = downloads_sender.send(downloads_page::DownloadsMsg::Refresh);
            }
            _ => {}
        }
    });

    let split_view = OverlaySplitView::new();
    split_view.set_min_sidebar_width(180.0);
    split_view.set_max_sidebar_width(230.0);
    split_view.set_show_sidebar(true);

    let sidebar = ViewSwitcherSidebar::builder().stack(&stack).build();
    let sidebar_header = HeaderBar::new();
    sidebar_header.set_title_widget(Some(&gtk4::Label::new(Some("Aileron"))));
    let sidebar_view = ToolbarView::new();
    sidebar_view.add_top_bar(&sidebar_header);
    sidebar_view.set_content(Some(&sidebar));

    let header = HeaderBar::new();
    let title = WindowTitle::builder()
        .title("Overview")
        .subtitle("Aileron")
        .build();
    let title_for_stack = title.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        title_for_stack.set_title(match stack.visible_child_name().as_deref() {
            Some("overview") => "Overview",
            Some("profiles") => "Profiles",
            Some("runtimes") => "Runtimes",
            Some("permissions") => "Permissions",
            Some("downloads") => "Downloads",
            Some("activity") => "Activity",
            _ => "Aileron",
        });
    });
    header.set_title_widget(Some(&title));

    let toolbar_view = ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&stack));

    split_view.set_sidebar(Some(&sidebar_view));
    split_view.set_content(Some(&toolbar_view));
    window.set_content(Some(&split_view));
}
