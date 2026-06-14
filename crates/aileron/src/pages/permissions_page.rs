/// Permissions page — per-app, per-use-case toggles with last-used timestamps.
use chrono::{DateTime, Local, TimeZone};
use gtk4::prelude::*;
use gtk4::{ListBox, ScrolledWindow};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, PreferencesGroup, PreferencesPage, SwitchRow};

pub fn build() -> gtk4::Widget {
    let page = PreferencesPage::new();

    let group = PreferencesGroup::new();
    group.set_title("App Permissions");

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

    refresh_permissions(&list_box);

    page.add(&group);
    page.upcast()
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
                return;
            }
            for perm in &reply.permissions {
                let row = SwitchRow::builder()
                    .title(&perm.use_case)
                    .active(perm.allowed)
                    .build();
                let subtitle = match &perm.last_used {
                    Some(lu) => format!(
                        "{} — last used (local): {}",
                        perm.app_id,
                        format_local_time(lu)
                    ),
                    None => perm.app_id.clone(),
                };
                row.set_subtitle(&subtitle);

                let app_id = perm.app_id.clone();
                let use_case = perm.use_case.clone();
                row.connect_active_notify(move |switch| {
                    use aileron_varlink::aileron_Permissions::VarlinkClientInterface;
                    let allowed = switch.is_active();
                    if let Ok(conn) = aileron_ipc::client::connect() {
                        let mut c = aileron_varlink::aileron_Permissions::VarlinkClient::new(conn);
                        let _ = c
                            .set_app_permission(app_id.clone(), use_case.clone(), allowed)
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

fn format_local_time(timestamp: &str) -> String {
    format_time_in(timestamp, &Local).unwrap_or_else(|| timestamp.to_string())
}

fn format_time_in<Tz>(timestamp: &str, timezone: &Tz) -> Option<String>
where
    Tz: TimeZone,
    Tz::Offset: std::fmt::Display,
{
    DateTime::parse_from_rfc3339(timestamp)
        .map(|dt| {
            dt.with_timezone(timezone)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    #[test]
    fn formats_rfc3339_timestamp_in_requested_timezone() {
        let timezone = FixedOffset::east_opt(2 * 60 * 60).unwrap();

        assert_eq!(
            format_time_in("2026-06-11T22:39:36Z", &timezone).as_deref(),
            Some("2026-06-12 00:39:36")
        );
    }

    #[test]
    fn preserves_unparseable_timestamps() {
        assert_eq!(format_local_time("not a timestamp"), "not a timestamp");
    }
}
