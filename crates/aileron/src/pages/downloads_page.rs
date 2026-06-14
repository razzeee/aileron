use std::cell::Cell;
use std::rc::Rc;

use aileron_varlink::aileron_Models::InstallStatus;
use gtk4::prelude::*;
use gtk4::{Box, Button, Label, ListBox, Orientation, ProgressBar, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, AlertDialog, PreferencesGroup, PreferencesPage};

#[derive(Clone)]
pub struct DownloadsView {
    pub widget: gtk4::Widget,
    list_box: ListBox,
    poll_active: Rc<Cell<bool>>,
}

impl DownloadsView {
    pub fn refresh(&self) {
        refresh_downloads_list(&self.list_box);
        if has_active_downloads() {
            self.start_poll();
        }
    }

    fn start_poll(&self) {
        if self.poll_active.get() {
            return;
        }
        self.poll_active.set(true);
        refresh_downloads_list(&self.list_box);

        let view = self.clone();
        let mut grace_ticks = 15;
        glib::timeout_add_seconds_local(2, move || {
            refresh_downloads_list(&view.list_box);
            if has_active_downloads() {
                grace_ticks = 15;
                glib::ControlFlow::Continue
            } else if grace_ticks > 0 {
                grace_ticks -= 1;
                glib::ControlFlow::Continue
            } else {
                view.poll_active.set(false);
                glib::ControlFlow::Break
            }
        });
    }
}

pub fn build() -> DownloadsView {
    let page = PreferencesPage::new();
    let group = PreferencesGroup::new();
    group.set_title("Downloads");
    group.set_description(Some(
        "Active profile installs, model artifact downloads, and runtime image pulls.",
    ));

    let list_box = ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let scroll = ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .min_content_height(300)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&list_box)
        .build();
    group.add(&scroll);
    page.add(&group);

    let view = DownloadsView {
        widget: page.upcast(),
        list_box,
        poll_active: Rc::new(Cell::new(false)),
    };
    view.refresh();
    view
}

fn refresh_downloads_list(list: &ListBox) {
    clear_list(list);

    use aileron_varlink::aileron_Models::VarlinkClientInterface;
    let installs = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client.list_installs().call().map_err(|e| e.to_string())
        });

    let installs = match installs {
        Ok(reply) => reply.installs,
        Err(reason) => {
            let row = ActionRow::new();
            row.set_title("Downloads unavailable");
            row.set_subtitle(&reason);
            list.append(&row);
            return;
        }
    };

    if installs.is_empty() {
        let row = ActionRow::new();
        row.set_title("No active downloads");
        row.set_subtitle(
            "Install and runtime image progress appears here while downloads are running.",
        );
        list.append(&row);
        return;
    }

    for install in installs {
        list.append(&download_row(&install, None));
    }
}

fn download_row(install: &InstallStatus, window: Option<gtk4::Window>) -> Box {
    let row = Box::new(Orientation::Horizontal, 12);
    row.set_margin_top(10);
    row.set_margin_bottom(10);
    row.set_margin_start(12);
    row.set_margin_end(12);

    let details = Box::new(Orientation::Vertical, 6);
    details.set_hexpand(true);

    let title = Label::new(Some(&download_title(&install.profile_id)));
    title.set_xalign(0.0);
    title.add_css_class("heading");

    let subtitle = Label::new(Some(&download_subtitle(
        install.bytes_pulled,
        install.total_bytes,
        install.bytes_per_second,
        install.eta_seconds,
        &install.status,
        install.cancel_requested,
    )));
    subtitle.set_xalign(0.0);
    subtitle.add_css_class("dim-label");

    let progress = ProgressBar::new();
    if install.total_bytes > 0 {
        progress.set_fraction(
            (install.bytes_pulled as f64 / install.total_bytes as f64).clamp(0.0, 1.0),
        );
    } else {
        progress.pulse();
    }

    details.append(&title);
    details.append(&subtitle);
    details.append(&progress);
    row.append(&details);

    if install_is_terminal(install) || is_runtime_download(&install.profile_id) {
        return row;
    }

    let cancel = Button::with_label("Cancel");
    cancel.set_valign(gtk4::Align::Center);
    cancel.set_sensitive(!install.cancel_requested);
    let profile_id = install.profile_id.clone();
    cancel.connect_clicked(move |btn| {
        btn.set_sensitive(false);
        let window = window
            .clone()
            .or_else(|| btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok()));
        cancel_install(&profile_id, window);
    });
    row.append(&cancel);
    row
}

fn download_title(id: &str) -> String {
    id.strip_prefix("runtime:")
        .map(|image_ref| format!("Runtime image: {image_ref}"))
        .unwrap_or_else(|| id.to_string())
}

fn is_runtime_download(id: &str) -> bool {
    id.starts_with("runtime:")
}

fn install_is_terminal(install: &InstallStatus) -> bool {
    install.status.starts_with("Failed:") || install.status == "Completed"
}

fn download_subtitle(
    bytes_pulled: i64,
    total_bytes: i64,
    bytes_per_second: i64,
    eta_seconds: i64,
    status: &str,
    cancelling: bool,
) -> String {
    if status.starts_with("Failed:") {
        return status.to_string();
    }

    let prefix = if cancelling { "Cancelling" } else { status };
    let speed = if bytes_per_second > 0 {
        format!(" · {}", format_speed(bytes_per_second))
    } else {
        " · speed calculating".to_string()
    };
    let eta = if eta_seconds >= 0 {
        format!(" · {} left", format_duration(eta_seconds))
    } else {
        String::new()
    };
    if total_bytes > 0 {
        format!(
            "{} · {:.1} / {:.1} GB{}{}",
            prefix,
            bytes_pulled as f64 / 1_000_000_000.0,
            total_bytes as f64 / 1_000_000_000.0,
            speed,
            eta,
        )
    } else {
        format!("{prefix} · size unknown{speed}")
    }
}

fn format_speed(bytes_per_second: i64) -> String {
    let bytes_per_second = bytes_per_second as f64;
    if bytes_per_second >= 1_000_000_000.0 {
        format!("{:.1} GB/s", bytes_per_second / 1_000_000_000.0)
    } else if bytes_per_second >= 1_000_000.0 {
        format!("{:.1} MB/s", bytes_per_second / 1_000_000.0)
    } else if bytes_per_second >= 1_000.0 {
        format!("{:.1} KB/s", bytes_per_second / 1_000.0)
    } else {
        format!("{} B/s", bytes_per_second as i64)
    }
}

fn format_duration(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

fn has_active_downloads() -> bool {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;

    aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client.list_installs().call().map_err(|e| e.to_string())
        })
        .map(|reply| {
            reply
                .installs
                .iter()
                .any(|install| !install_is_terminal(install))
        })
        .unwrap_or(false)
}

fn cancel_install(profile_id: &str, window: Option<gtk4::Window>) {
    use aileron_varlink::aileron_Models::VarlinkClientInterface;

    let result = aileron_ipc::client::connect()
        .map_err(|e| e.to_string())
        .and_then(|conn| {
            let mut client = aileron_varlink::aileron_Models::VarlinkClient::new(conn);
            client
                .cancel_install(profile_id.to_string())
                .call()
                .map_err(|e| e.to_string())
        });

    if let Err(reason) = result {
        let dialog = AlertDialog::builder()
            .heading("Cancel failed")
            .body(&reason)
            .build();
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.present(window.as_ref());
    }
}

fn clear_list(list: &ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}
