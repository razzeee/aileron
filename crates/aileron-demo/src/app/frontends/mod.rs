pub(super) mod chat;
pub(super) mod embedding;
pub(super) mod overview;
pub(super) mod prediction;
pub(super) mod speech;
pub(super) mod text;
pub(super) mod tool;
pub(super) mod vision;

use gtk4::ScrolledWindow;
use gtk4::prelude::*;

pub(super) fn scrollable_page<W: IsA<gtk4::Widget>>(child: &W) -> gtk4::Widget {
    ScrolledWindow::builder()
        .child(child)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .hexpand(true)
        .vexpand(true)
        .build()
        .upcast()
}
