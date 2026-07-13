use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, CssProvider, Image, Label, Orientation};
use libadwaita::ViewStack;

pub(crate) fn build_page(stack: &ViewStack) -> gtk4::Widget {
    install_overview_css();

    let root = Box::new(Orientation::Vertical, 18);
    root.set_margin_top(22);
    root.set_margin_bottom(22);
    root.set_margin_start(22);
    root.set_margin_end(22);

    let hero = Box::new(Orientation::Vertical, 12);
    hero.add_css_class("overview-hero");
    hero.set_margin_bottom(4);

    let eyebrow = Label::builder()
        .label("LOCAL AI PORTAL TEST RANGE")
        .xalign(0.0)
        .css_classes(vec!["caption", "overview-kicker"])
        .build();
    let title = Label::builder()
        .label("One sandbox. Every capability. No cloud detours.")
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["title-1"])
        .build();
    let subtitle = Label::builder()
        .label("Aileron Demo is a local control room for exercising text, chat, tools, speech, vision, and embeddings through the portal surface.")
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["overview-hero-copy"])
        .build();

    let stats = Box::new(Orientation::Horizontal, 8);
    stats.append(&stat_pill("6", "portal paths"));
    stats.append(&stat_pill("0", "required cloud calls"));

    hero.append(&eyebrow);
    hero.append(&title);
    hero.append(&subtitle);
    hero.append(&stats);
    root.append(&hero);

    root.append(
        &Label::builder()
            .label("Choose a launch lane")
            .xalign(0.0)
            .css_classes(vec!["heading"])
            .build(),
    );

    let cards = Box::new(Orientation::Vertical, 12);
    let labs = [
        LabCard {
            title: "Chat Lab",
            subtitle: "Run guided chat turns, then verify local session memory across follow-ups.",
            methods: "CreateSession, StreamRespondGuided, Session.Close",
            example: "Try: tell it a preference, then ask a follow-up that uses memory.",
            button_label: "Open Chat Lab",
            page_name: "chat",
            icon_name: "user-available-symbolic",
        },
        LabCard {
            title: "Text Lab",
            subtitle: "Fetch or paste text, then summarize, translate, rephrase, classify, extract JSON, or analyze.",
            methods: "StreamResponse, StreamRespondGuided",
            example: "Try: paste an article, classify it, then extract JSON facts.",
            button_label: "Open Text Lab",
            page_name: "text",
            icon_name: "text-x-generic-symbolic",
        },
        LabCard {
            title: "Tool Lab",
            subtitle: "Run tiny agent loops where the model asks for app-owned tools, including whole-PC Linux diagnostics.",
            methods: "CreateSession, StreamRespondGuided, Session.Close",
            example: "Try: collect read-only PC diagnostics and ask for safe bugfix guidance.",
            button_label: "Open Tool Lab",
            page_name: "tools",
            icon_name: "applications-system-symbolic",
        },
        LabCard {
            title: "Speech Lab",
            subtitle: "Record microphone audio and stream transcription or translation through the Speech portal path.",
            methods: "StreamTranscribe",
            example: "Try: start Live Transcribe, speak for 5-10 seconds, then stop for the final pass.",
            button_label: "Open Speech Lab",
            page_name: "speech",
            icon_name: "audio-input-microphone-symbolic",
        },
        LabCard {
            title: "Vision Lab",
            subtitle: "Choose an image file and run description or segmentation through the vision portal path.",
            methods: "StreamDescribe, StreamSegment",
            example: "Try: choose a screenshot, describe it, then segment visible objects.",
            button_label: "Open Vision Lab",
            page_name: "vision",
            icon_name: "image-x-generic-symbolic",
        },
        LabCard {
            title: "Embeddings",
            subtitle: "Generate dense vectors for short text and inspect the numerical output without leaving the sandbox.",
            methods: "Embed",
            example: "Try: compare two short snippets and inspect their vector shapes.",
            button_label: "Open Embeddings",
            page_name: "embed",
            icon_name: "view-grid-symbolic",
        },
    ];
    for lab in labs {
        cards.append(&lab_card(lab, stack));
    }
    root.append(&cards);

    scrollable_page(&root)
}

fn install_overview_css() {
    let Some(display) = gtk4::gdk::Display::default() else {
        return;
    };

    let provider = CssProvider::new();
    provider.load_from_string(
        r#"
        .overview-hero {
            background: linear-gradient(135deg, alpha(@accent_bg_color, 0.42), alpha(@card_bg_color, 0.96) 58%, alpha(@accent_bg_color, 0.10));
            border: 1px solid alpha(@accent_bg_color, 0.38);
            border-radius: 28px;
            padding: 24px;
        }

        .overview-kicker {
            color: @accent_color;
            font-weight: 700;
            letter-spacing: 0.12em;
        }

        .overview-hero-copy {
            color: alpha(currentColor, 0.74);
            font-size: 1.08em;
        }

        .overview-pill {
            background: alpha(@window_bg_color, 0.58);
            border: 1px solid alpha(currentColor, 0.12);
            border-radius: 999px;
            padding: 7px 11px;
        }

        .overview-card:hover {
            background: alpha(@accent_bg_color, 0.12);
        }

        .overview-icon-badge {
            background: alpha(@accent_bg_color, 0.18);
            border-radius: 18px;
            padding: 11px;
            color: @accent_color;
        }

        "#,
    );
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn stat_pill(value: &str, label: &str) -> Box {
    let pill = Box::new(Orientation::Horizontal, 5);
    pill.add_css_class("overview-pill");
    pill.set_valign(Align::Start);
    pill.append(
        &Label::builder()
            .label(value)
            .css_classes(vec!["heading"])
            .build(),
    );
    pill.append(
        &Label::builder()
            .label(label)
            .css_classes(vec!["caption", "dim-label"])
            .build(),
    );
    pill
}

struct LabCard {
    title: &'static str,
    subtitle: &'static str,
    methods: &'static str,
    example: &'static str,
    button_label: &'static str,
    page_name: &'static str,
    icon_name: &'static str,
}

fn lab_card(lab: LabCard, stack: &ViewStack) -> Box {
    let card = Box::new(Orientation::Horizontal, 14);
    card.add_css_class("card");
    card.add_css_class("overview-card");
    card.set_height_request(132);
    card.set_margin_top(2);
    card.set_margin_bottom(2);
    card.set_margin_start(2);
    card.set_margin_end(2);

    let icon_box = Box::new(Orientation::Vertical, 0);
    icon_box.add_css_class("overview-icon-badge");
    icon_box.set_margin_top(14);
    icon_box.set_margin_bottom(14);
    icon_box.set_margin_start(14);
    icon_box.set_valign(Align::Start);
    let icon = Image::from_icon_name(lab.icon_name);
    icon.set_pixel_size(28);
    icon_box.append(&icon);

    let content = Box::new(Orientation::Vertical, 6);
    content.set_hexpand(true);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(0);
    content.set_margin_end(8);

    let title = Label::builder()
        .label(lab.title)
        .xalign(0.0)
        .css_classes(vec!["heading"])
        .build();
    let subtitle = Label::builder()
        .label(lab.subtitle)
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["dim-label"])
        .build();
    let methods = Label::builder()
        .label(format!("Portal: {}", lab.methods))
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["caption", "dim-label"])
        .build();
    let example = Label::builder()
        .label(lab.example)
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["caption"])
        .build();
    content.append(&title);
    content.append(&subtitle);
    content.append(&methods);
    content.append(&example);

    let action_box = Box::new(Orientation::Vertical, 0);
    action_box.set_margin_top(12);
    action_box.set_margin_bottom(12);
    action_box.set_margin_end(14);
    action_box.set_valign(Align::Center);
    let button = Button::builder()
        .label(lab.button_label)
        .css_classes(vec!["suggested-action"])
        .build();
    {
        let stack = stack.clone();
        button.connect_clicked(move |_| {
            stack.set_visible_child_name(lab.page_name);
        });
    }
    action_box.append(&button);

    card.append(&icon_box);
    card.append(&content);
    card.append(&action_box);
    card
}
