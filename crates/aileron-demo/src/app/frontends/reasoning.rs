use super::super::{
    LANGUAGE_IFACE, PORTAL_BUS, PORTAL_PATH, PortalOptions, REQUEST_IFACE, close_public_request,
    close_public_session, create_public_session, execution_options, friendly_error,
    portal_connection, portal_request_path, string_option_value, text_shorthand_json,
    wait_request_response_from_iter,
};
use gtk4::prelude::*;
use gtk4::{
    Box, Button, CheckButton, DropDown, Label, Orientation, ScrolledWindow, SpinButton, StringList,
    StringObject, TextBuffer, TextView,
};
use std::sync::{Arc, Mutex, mpsc};
use zbus::zvariant::{OwnedFd, OwnedObjectPath};

#[derive(Default)]
struct State {
    session: Option<OwnedObjectPath>,
    request: Option<OwnedObjectPath>,
}

enum Event {
    Capabilities(Vec<String>, Vec<String>, bool),
    Reasoning(String),
    Answer(String),
    Finished(String),
    Error(String),
}

fn request_options(
    connection: &zbus::blocking::Connection,
    prefix: &str,
) -> anyhow::Result<(OwnedObjectPath, PortalOptions)> {
    let token = format!(
        "{prefix}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let path = OwnedObjectPath::try_from(portal_request_path(connection, &token)?)?;
    let mut options = execution_options();
    options.insert("handle_token".into(), string_option_value(&token));
    Ok((path, options))
}

fn discover(state: &Arc<Mutex<State>>, tx: &mpsc::Sender<Event>) -> anyhow::Result<()> {
    let connection = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&connection, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let version: u32 = proxy.get_property("version")?;
    anyhow::ensure!(
        version >= 2,
        "The Language portal needs version 2 for reasoning controls."
    );
    let old = state.lock().unwrap().session.take();
    if let Some(session) = old {
        let _ = close_public_session(&session);
    }
    let session =
        create_public_session(&proxy, "language.analyze", "Answer clearly and accurately.")?;
    state.lock().unwrap().session = Some(session.clone());
    let (path, mut options) = request_options(&connection, "reasoning_caps")?;
    options.remove("execution_mode");
    state.lock().unwrap().request = Some(path.clone());
    let request =
        zbus::blocking::Proxy::new(&connection, PORTAL_BUS, path.as_str(), REQUEST_IFACE)?;
    let mut response = request.receive_signal("Response")?;
    let returned: OwnedObjectPath = proxy.call("GetReasoningCapabilities", &(&session, options))?;
    anyhow::ensure!(returned == path, "unexpected capability request handle");
    let mut result = wait_request_response_from_iter(&mut response)?;
    let modes = Vec::<String>::try_from(
        result
            .remove("thinking_modes")
            .ok_or_else(|| anyhow::anyhow!("missing thinking modes"))?,
    )?;
    let efforts = Vec::<String>::try_from(
        result
            .remove("reasoning_efforts")
            .ok_or_else(|| anyhow::anyhow!("missing effort levels"))?,
    )?;
    let output = bool::try_from(
        result
            .remove("reasoning_output")
            .ok_or_else(|| anyhow::anyhow!("missing reasoning output capability"))?,
    )?;
    tx.send(Event::Capabilities(modes, efforts, output))?;
    Ok(())
}

fn generate(
    state: &Arc<Mutex<State>>,
    prompt: String,
    thinking: Option<String>,
    effort: Option<String>,
    include: bool,
    budget: i64,
    tx: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    let connection = portal_connection()?;
    let session = state
        .lock()
        .unwrap()
        .session
        .clone()
        .ok_or_else(|| anyhow::anyhow!("Load capabilities first"))?;
    let proxy = zbus::blocking::Proxy::new(&connection, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let (path, mut options) = request_options(&connection, "reasoning_run")?;
    state.lock().unwrap().request = Some(path.clone());
    options.insert("maximum_response_tokens".into(), budget.into());
    options.insert("include_reasoning".into(), include.into());
    if let Some(mode) = thinking {
        options.insert("thinking".into(), string_option_value(&mode));
    }
    if let Some(effort) = effort {
        options.insert("reasoning_effort".into(), string_option_value(&effort));
    }
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(PORTAL_BUS)?
        .path_namespace(PORTAL_PATH)?
        .build();
    let messages = zbus::blocking::MessageIterator::for_match_rule(rule, &connection, Some(1024))?;
    let returned: OwnedObjectPath = proxy.call(
        "StreamResponse",
        &(
            &session,
            text_shorthand_json(&prompt),
            Vec::<OwnedFd>::new(),
            options,
        ),
    )?;
    anyhow::ensure!(returned == path, "unexpected generation request handle");
    for message in messages {
        let message = message?;
        let header = message.header();
        let member = header.member().map(|v| v.as_str()).unwrap_or_default();
        if header.path().is_some_and(|p| p.as_str() == path.as_str()) && member == "Response" {
            let (code, values): (u32, PortalOptions) = message.body().deserialize()?;
            if code != 0 {
                anyhow::bail!(
                    "{}",
                    values
                        .get("error")
                        .and_then(|v| <&str>::try_from(v).ok())
                        .unwrap_or("Request failed or was cancelled")
                );
            }
            let reason = values
                .get("finish_reason")
                .and_then(|v| <&str>::try_from(v).ok())
                .unwrap_or("completed");
            tx.send(Event::Finished(format!(
                "Finish: {reason}. Usage: {:?}",
                values.get("usage")
            )))?;
            return Ok(());
        }
        if header
            .interface()
            .is_none_or(|i| i.as_str() != LANGUAGE_IFACE)
        {
            continue;
        }
        if member == "ReasoningReceived" {
            let (request, owner, text): (OwnedObjectPath, OwnedObjectPath, String) =
                message.body().deserialize()?;
            if request == path && owner == session {
                tx.send(Event::Reasoning(text))?;
            }
        } else if member == "TokenReceived" {
            let (request, owner, text, _): (OwnedObjectPath, OwnedObjectPath, String, bool) =
                message.body().deserialize()?;
            if request == path && owner == session {
                tx.send(Event::Answer(text))?;
            }
        }
    }
    anyhow::bail!("Portal closed before completing the request")
}

fn selection(dropdown: &DropDown) -> Option<String> {
    if dropdown.selected() == 0 {
        return None;
    }
    dropdown
        .selected_item()
        .and_downcast::<StringObject>()
        .map(|item| item.string().to_string())
}

pub(crate) fn build_page() -> gtk4::Widget {
    let root = Box::new(Orientation::Vertical, 12);
    for setter in [
        Box::set_margin_top,
        Box::set_margin_bottom,
        Box::set_margin_start,
        Box::set_margin_end,
    ] {
        setter(&root, 12);
    }
    let load = Button::with_label("Load model capabilities");
    let modes = DropDown::from_strings(&["Default"]);
    let efforts = DropDown::from_strings(&["Default"]);
    modes.set_sensitive(false);
    efforts.set_sensitive(false);
    let include = CheckButton::with_label("Show reasoning separately");
    include.set_sensitive(false);
    let budget = SpinButton::with_range(1.0, 32768.0, 128.0);
    budget.set_value(2048.0);
    let status = Label::builder()
        .label("Load capabilities for the assigned language.analyze model.")
        .wrap(true)
        .xalign(0.0)
        .build();
    root.append(&load);
    root.append(&status);
    for (label, widget) in [
        ("Thinking", modes.clone().upcast::<gtk4::Widget>()),
        ("Effort", efforts.clone().upcast()),
        ("Total output tokens", budget.clone().upcast()),
    ] {
        let row = Box::new(Orientation::Horizontal, 8);
        row.append(&Label::new(Some(label)));
        row.append(&widget);
        root.append(&row);
    }
    root.append(&include);
    root.append(
        &Label::builder()
            .label("The token budget includes both reasoning and the answer.")
            .xalign(0.0)
            .build(),
    );
    let input = TextBuffer::new(None);
    input.set_text("Explain how you would check whether a proposed solution is correct.");
    let reasoning = TextBuffer::new(None);
    let answer = TextBuffer::new(None);
    for (label, buffer, editable) in [
        ("Prompt", &input, true),
        ("Reasoning", &reasoning, false),
        ("Answer", &answer, false),
    ] {
        root.append(&Label::builder().label(label).xalign(0.0).build());
        let view = TextView::builder()
            .buffer(buffer)
            .editable(editable)
            .wrap_mode(gtk4::WrapMode::WordChar)
            .build();
        root.append(
            &ScrolledWindow::builder()
                .child(&view)
                .min_content_height(120)
                .vexpand(true)
                .build(),
        );
    }
    let row = Box::new(Orientation::Horizontal, 8);
    let run = Button::with_label("Run");
    run.set_sensitive(false);
    let cancel = Button::with_label("Cancel");
    cancel.set_sensitive(false);
    row.append(&run);
    row.append(&cancel);
    root.append(&row);
    let state = Arc::new(Mutex::new(State::default()));
    let (tx, rx) = mpsc::channel();
    {
        let state = state.clone();
        let tx = tx.clone();
        let cancel = cancel.clone();
        let run = run.clone();
        let status = status.clone();
        load.connect_clicked(move |button| {
            button.set_sensitive(false);
            run.set_sensitive(false);
            cancel.set_sensitive(true);
            status.set_text("Preparing model and checking capabilities…");
            let state = state.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(error) = discover(&state, &tx) {
                    if let Some(session) = state.lock().unwrap().session.take() {
                        let _ = close_public_session(&session);
                    }
                    let _ = tx.send(Event::Error(friendly_error(&error)));
                }
                state.lock().unwrap().request = None;
            });
        });
    }
    {
        let state = state.clone();
        let tx = tx.clone();
        let load = load.clone();
        let cancel = cancel.clone();
        let status = status.clone();
        let modes = modes.clone();
        let efforts = efforts.clone();
        let include = include.clone();
        let reasoning = reasoning.clone();
        let answer = answer.clone();
        run.connect_clicked(move |button| {
            let prompt = input
                .text(&input.start_iter(), &input.end_iter(), false)
                .to_string();
            if prompt.trim().is_empty() {
                return;
            }
            let thinking = selection(&modes);
            let effort = if thinking.as_deref() == Some("off") {
                None
            } else {
                selection(&efforts)
            };
            let show = include.is_active();
            let tokens = budget.value_as_int() as i64;
            reasoning.set_text("");
            answer.set_text("");
            button.set_sensitive(false);
            load.set_sensitive(false);
            cancel.set_sensitive(true);
            status.set_text("Generating…");
            let state = state.clone();
            let tx = tx.clone();
            std::thread::spawn(move || {
                if let Err(error) = generate(&state, prompt, thinking, effort, show, tokens, &tx) {
                    if let Some(request) = state.lock().unwrap().request.clone() {
                        let _ = close_public_request(&request);
                    }
                    let _ = tx.send(Event::Error(friendly_error(&error)));
                }
                state.lock().unwrap().request = None;
            });
        });
    }
    {
        let state = state.clone();
        cancel.connect_clicked(move |button| {
            button.set_sensitive(false);
            if let Some(request) = state.lock().unwrap().request.clone() {
                std::thread::spawn(move || {
                    let _ = close_public_request(&request);
                });
            }
        });
    }
    {
        let state = state.clone();
        root.connect_unrealize(move |_| {
            if let Some(session) = state.lock().unwrap().session.take() {
                std::thread::spawn(move || {
                    let _ = close_public_session(&session);
                });
            }
        });
    }
    let root_weak = root.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
        if root_weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Capabilities(thinking, levels, output) => {
                    let names = std::iter::once("Default".to_string())
                        .chain(thinking)
                        .collect::<Vec<_>>();
                    modes.set_model(Some(&StringList::new(
                        &names.iter().map(String::as_str).collect::<Vec<_>>(),
                    )));
                    modes.set_selected(0);
                    modes.set_sensitive(names.len() > 1);
                    let names = std::iter::once("Default".to_string())
                        .chain(levels)
                        .collect::<Vec<_>>();
                    efforts.set_model(Some(&StringList::new(
                        &names.iter().map(String::as_str).collect::<Vec<_>>(),
                    )));
                    efforts.set_selected(0);
                    efforts.set_sensitive(names.len() > 1);
                    include.set_sensitive(output);
                    include.set_active(output);
                    status.set_text("Capabilities loaded. Only advertised controls are enabled.");
                    load.set_sensitive(true);
                    run.set_sensitive(true);
                    cancel.set_sensitive(false);
                }
                Event::Reasoning(text) => reasoning.insert(&mut reasoning.end_iter(), &text),
                Event::Answer(text) => answer.insert(&mut answer.end_iter(), &text),
                Event::Finished(message) | Event::Error(message) => {
                    status.set_text(&message);
                    load.set_sensitive(true);
                    run.set_sensitive(state.lock().unwrap().session.is_some());
                    cancel.set_sensitive(false);
                }
            }
        }
        glib::ControlFlow::Continue
    });
    super::scrollable_page(&root)
}
