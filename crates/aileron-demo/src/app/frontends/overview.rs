use super::scrollable_page;
use gtk4::prelude::*;
use gtk4::{Align, Box, Button, Label, Orientation};
use libadwaita::ViewStack;

pub(crate) fn build_page(stack: &ViewStack) -> gtk4::Widget {
    let root = Box::new(Orientation::Vertical, 16);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    root.append(
        &Label::builder()
            .label("Try each Aileron portal capability from one local sandboxed app.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(vec!["title-2"])
            .build(),
    );

    let cards = Box::new(Orientation::Vertical, 12);
    cards.append(&lab_card(
        "Chat Lab",
        "Run chat-shaped turns through guided language.extract responses with local memory.",
        "RespondGuided, CreateSession, EndSession",
        "Try: tell it a preference, then ask a follow-up that uses memory.",
        "Open Chat Lab",
        "chat",
        stack,
    ));
    cards.append(&lab_card(
        "Text Lab",
        "Fetch or paste text, then summarize, translate, rephrase, classify, extract JSON, or analyze.",
        "StreamResponse, Respond, RespondGuided",
        "Try: paste an article, classify it, then extract JSON facts.",
        "Open Text Lab",
        "text",
        stack,
    ));
    cards.append(&lab_card(
        "Prediction Lab",
        "Type a sentence and preview a short ghost continuation from the local language model.",
        "PredictNext",
        "Try: The old lighthouse keeper opened the door and",
        "Open Prediction Lab",
        "predict",
        stack,
    ));
    cards.append(&lab_card(
        "Tool Lab",
        "Run a tiny agent loop where the model asks for an app-owned deterministic tool.",
        "CreateSession, RespondGuided, EndSession",
        "Try: ask how many r's are in strawrberrry and watch the app loop decide when to run the tool.",
        "Open Tool Lab",
        "tools",
        stack,
    ));
    cards.append(&lab_card(
        "Speech Lab",
        "Record microphone audio and stream transcription or translation through the Speech portal path.",
        "Transcribe, StreamTranscribe",
        "Try: start Live Transcribe, speak for 5-10 seconds, then stop for the final pass.",
        "Open Speech Lab",
        "speech",
        stack,
    ));
    cards.append(&lab_card(
        "Vision Lab",
        "Choose or paste an image and run description or segmentation through the vision portal path.",
        "Describe, Segment",
        "Try: choose a screenshot, describe it, then segment visible objects.",
        "Open Vision Lab",
        "vision",
        stack,
    ));
    root.append(&cards);

    scrollable_page(&root)
}

fn lab_card(
    title: &str,
    subtitle: &str,
    methods: &str,
    example: &str,
    button_label: &str,
    page_name: &'static str,
    stack: &ViewStack,
) -> Box {
    let card = Box::new(Orientation::Horizontal, 14);
    card.add_css_class("card");
    card.set_height_request(132);
    card.set_margin_top(2);
    card.set_margin_bottom(2);
    card.set_margin_start(2);
    card.set_margin_end(2);

    let content = Box::new(Orientation::Vertical, 6);
    content.set_hexpand(true);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(14);
    content.set_margin_end(8);

    let title = Label::builder()
        .label(title)
        .xalign(0.0)
        .css_classes(vec!["heading"])
        .build();
    let subtitle = Label::builder()
        .label(subtitle)
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["dim-label"])
        .build();
    let methods = Label::builder()
        .label(format!("Portal: {methods}"))
        .xalign(0.0)
        .wrap(true)
        .css_classes(vec!["caption", "dim-label"])
        .build();
    let example = Label::builder()
        .label(example)
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
        .label(button_label)
        .css_classes(vec!["suggested-action"])
        .build();
    {
        let stack = stack.clone();
        button.connect_clicked(move |_| {
            stack.set_visible_child_name(page_name);
        });
    }
    action_box.append(&button);

    card.append(&content);
    card.append(&action_box);
    card
}
