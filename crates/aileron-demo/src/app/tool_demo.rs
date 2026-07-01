use super::{
    LANGUAGE_IFACE, PORTAL_BUS, PORTAL_PATH, ToolDefinitionDbus, ToolResultDbus,
    close_public_session, create_public_session, generation_options, portal_connection,
    stream_guided_response, stream_guided_tool_results, stream_language_text,
};
use serde::Deserialize;
use zbus::zvariant::OwnedObjectPath;

pub(crate) enum ToolEvent {
    Trace(String),
    ConfirmationRequested {
        tool_name: String,
        arguments_json: String,
        response_tx: std::sync::mpsc::Sender<bool>,
    },
    Final(String),
    Cancelled(String),
    Error(String),
    Done,
}

#[derive(Clone, Copy)]
pub(crate) enum ToolDemoCase {
    CharacterCounter,
    LinuxDiagnostics,
}

impl ToolDemoCase {
    pub(crate) fn labels() -> [&'static str; 2] {
        [
            ToolDemoCase::CharacterCounter.label(),
            ToolDemoCase::LinuxDiagnostics.label(),
        ]
    }

    pub(crate) fn index(&self) -> u32 {
        match self {
            ToolDemoCase::CharacterCounter => 0,
            ToolDemoCase::LinuxDiagnostics => 1,
        }
    }

    pub(crate) fn from_index(index: u32) -> Option<Self> {
        match index {
            0 => Some(ToolDemoCase::CharacterCounter),
            1 => Some(ToolDemoCase::LinuxDiagnostics),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            ToolDemoCase::CharacterCounter => "Character counter",
            ToolDemoCase::LinuxDiagnostics => "Linux PC diagnostics",
        }
    }

    pub(crate) fn default_prompt(&self) -> &'static str {
        match self {
            ToolDemoCase::CharacterCounter => {
                "How many times does the letter r occur in strawrberrry?"
            }
            ToolDemoCase::LinuxDiagnostics => {
                "Analyze this Linux PC for recent failures or resource problems and recommend safe bugfix steps."
            }
        }
    }

    pub(crate) fn ready_detail(&self) -> &'static str {
        match self {
            ToolDemoCase::CharacterCounter => {
                "Run the deterministic character-counter tool through the Language portal."
            }
            ToolDemoCase::LinuxDiagnostics => {
                "Collect read-only Linux PC diagnostics locally, then ask the model for fix guidance."
            }
        }
    }

    pub(crate) fn running_detail(&self) -> &'static str {
        match self {
            ToolDemoCase::CharacterCounter => "The app owns the loop and executes tools locally.",
            ToolDemoCase::LinuxDiagnostics => {
                "The app is waiting for approval before collecting bounded, read-only PC diagnostics."
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct GuidedToolLoopResponse {
    action: String,
    tool_name: String,
    word: String,
    character: String,
    #[serde(default)]
    answer: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GuidedDiagnosticsLoopResponse {
    #[serde(default)]
    action: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    lines: Option<i64>,
    #[serde(default)]
    tool_input: String,
    #[serde(default)]
    answer: String,
}

#[derive(Clone)]
struct LinuxDiagnosticsPlan {
    scope: String,
    unit: Option<String>,
    lines: u64,
    commands: Vec<DiagnosticCommand>,
}

#[derive(Clone)]
struct DiagnosticCommand {
    label: String,
    program: String,
    args: Vec<String>,
}

pub(crate) fn run_tool_demo(
    case: ToolDemoCase,
    prompt: &str,
    tx: std::sync::mpsc::Sender<ToolEvent>,
) -> anyhow::Result<()> {
    match case {
        ToolDemoCase::CharacterCounter => run_character_tool_demo(prompt, tx),
        ToolDemoCase::LinuxDiagnostics => run_linux_diagnostics_tool_demo(prompt, tx),
    }
}

fn request_tool_confirmation(
    tx: &std::sync::mpsc::Sender<ToolEvent>,
    tool_name: &str,
    arguments_json: &str,
) -> anyhow::Result<bool> {
    let (response_tx, response_rx) = std::sync::mpsc::channel();
    tx.send(ToolEvent::ConfirmationRequested {
        tool_name: tool_name.to_string(),
        arguments_json: arguments_json.to_string(),
        response_tx,
    })?;
    response_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("tool confirmation dialog closed unexpectedly"))
}

fn cancel_tool_execution(
    tx: &std::sync::mpsc::Sender<ToolEvent>,
    session_handle: &OwnedObjectPath,
    tool_name: &str,
) -> anyhow::Result<()> {
    tx.send(ToolEvent::Trace(format!(
        "before_tool_execution: user rejected {tool_name}; no local tool ran"
    )))?;
    let _ = close_public_session(session_handle);
    tx.send(ToolEvent::Cancelled(format!(
        "The {tool_name} tool call was cancelled before local execution."
    )))?;
    Ok(())
}

fn run_character_tool_demo(
    prompt: &str,
    tx: std::sync::mpsc::Sender<ToolEvent>,
) -> anyhow::Result<()> {
    tx.send(ToolEvent::Trace(
        "before_agent_loop: seed messages and register count_character_occurrences".to_string(),
    ))?;

    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let session_handle = create_public_session(
        &proxy,
        "language.analyze",
        "You are a small local agent. Return guided JSON. Use action=call_tool when exact character counting is needed. Use action=final only after tool_result is provided.",
    )?;

    let fields = guided_tool_loop_fields();
    let tools = count_tool_definitions()?;
    let options = generation_options(128, "", "");
    let mut loop_prompt = format!(
        "Available app tool:\n- count_character_occurrences(word: string, character: string): exact deterministic count.\n\nUser request: {prompt}\n\nReturn action=call_tool with tool_name=count_character_occurrences, word, and character if this needs exact counting. Return action=final only if no tool is needed."
    );

    tx.send(ToolEvent::Trace(
        "before_llm_call: ask StreamRespondGuided for app-loop action".to_string(),
    ))?;
    let (content, tool_calls) = stream_guided_response(
        &session_handle,
        &loop_prompt,
        fields.clone(),
        tools.clone(),
        options.clone(),
    )?;
    let mut response = if content.trim().is_empty() && !tool_calls.is_empty() {
        GuidedToolLoopResponse {
            action: "call_tool".to_string(),
            tool_name: "count_character_occurrences".to_string(),
            word: String::new(),
            character: String::new(),
            answer: String::new(),
        }
    } else {
        parse_guided_tool_loop_response(&content, prompt, false)?
    };
    tx.send(ToolEvent::Trace(format!(
        "after_llm_call: action={}, native_tool_calls={}, tool_name={}, word={}, character={}, answer={:?}",
        response.action,
        tool_calls.len(),
        response.tool_name,
        response.word,
        response.character,
        response.answer
    )))?;

    if response.action != "call_tool" && tool_calls.is_empty() {
        tx.send(ToolEvent::Trace(
            "after_agent_loop: guided response selected final answer without tool execution"
                .to_string(),
        ))?;
        let answer = match initial_final_answer(response) {
            Ok(answer) => answer,
            Err(e) => {
                close_public_session(&session_handle)?;
                return Err(e);
            }
        };
        tx.send(ToolEvent::Final(answer))?;
        close_public_session(&session_handle)?;
        tx.send(ToolEvent::Done)?;
        return Ok(());
    }

    let mut results = Vec::new();
    let result_json = if tool_calls.is_empty() {
        let args = serde_json::json!({
            "word": response.word,
            "character": response.character
        });
        let arguments_json = args.to_string();
        tx.send(ToolEvent::Trace(format!(
            "before_tool_execution: count_character_occurrences args={arguments_json}; awaiting user approval"
        )))?;
        if !request_tool_confirmation(&tx, "count_character_occurrences", &arguments_json)? {
            return cancel_tool_execution(&tx, &session_handle, "count_character_occurrences");
        }
        tx.send(ToolEvent::Trace(
            "before_tool_execution: user approved count_character_occurrences".to_string(),
        ))?;
        let result_json = execute_count_tool(prompt, &arguments_json)?;
        tx.send(ToolEvent::Trace(format!(
            "after_tool_execution: result={result_json}"
        )))?;
        result_json
    } else {
        let mut last_result = serde_json::Value::Null;
        for call in tool_calls {
            tx.send(ToolEvent::Trace(format!(
                "before_tool_execution: {} id={} args={}; awaiting user approval",
                call.name, call.id, call.arguments_json
            )))?;
            if !request_tool_confirmation(&tx, &call.name, &call.arguments_json)? {
                return cancel_tool_execution(&tx, &session_handle, &call.name);
            }
            tx.send(ToolEvent::Trace(format!(
                "before_tool_execution: user approved {} id={}",
                call.name, call.id
            )))?;
            let result_json = execute_count_tool(prompt, &call.arguments_json)?;
            tx.send(ToolEvent::Trace(format!(
                "after_tool_execution: result={result_json}"
            )))?;
            results.push(ToolResultDbus {
                id: call.id,
                content: result_json.to_string(),
                content_json: result_json.to_string(),
            });
            last_result = result_json;
        }
        last_result
    };

    tx.send(ToolEvent::Trace(
        "before_llm_call: append tool_result to prompt and stream guided response again"
            .to_string(),
    ))?;
    loop_prompt.push_str("\n\ntool_result from count_character_occurrences:\n");
    loop_prompt.push_str(&result_json.to_string());
    loop_prompt.push_str("\n\nNow return action=final and put the user-facing answer in answer.");
    let final_content = if results.is_empty() {
        let (content, _) =
            stream_guided_response(&session_handle, &loop_prompt, fields, tools, options)?;
        content
    } else {
        let (content, _) = stream_guided_tool_results(
            &session_handle,
            &loop_prompt,
            results,
            fields,
            tools,
            options,
        )?;
        content
    };
    response = parse_guided_tool_loop_response(&final_content, prompt, true)?;
    tx.send(ToolEvent::Trace(format!(
        "after_llm_call: action={}, answer={:?}",
        response.action, response.answer
    )))?;
    tx.send(ToolEvent::Trace(
        "after_agent_loop: stop after one guided tool round for this demo".to_string(),
    ))?;
    tx.send(ToolEvent::Final(
        if response.answer.trim().is_empty() || response.answer == "stub" {
            format_tool_result_answer(&result_json)
        } else {
            response.answer
        },
    ))?;

    close_public_session(&session_handle)?;
    tx.send(ToolEvent::Done)?;
    Ok(())
}

fn run_linux_diagnostics_tool_demo(
    prompt: &str,
    tx: std::sync::mpsc::Sender<ToolEvent>,
) -> anyhow::Result<()> {
    tx.send(ToolEvent::Trace(
        "before_agent_loop: seed messages and register collect_linux_pc_diagnostics".to_string(),
    ))?;

    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let session_handle = create_public_session(
        &proxy,
        "language.analyze",
        "You are a local Linux PC diagnostics assistant. Use the app-provided read-only diagnostics tool before answering. Recommend safe bugfix steps, but do not claim that you changed the machine.",
    )?;

    let fields = guided_linux_pc_diagnostics_loop_fields();
    let tools = linux_pc_diagnostics_tool_definitions()?;
    let options = generation_options(384, "", "");
    let loop_prompt = format!(
        "Available app tool:\n- collect_linux_pc_diagnostics(scope?: all|user|system, unit?: systemd unit, lines?: number): collect bounded read-only Linux PC status, resource usage, failed units, kernel messages, and journal excerpts from the local machine.\n\nPolicy:\n- The app may read local status and logs only.\n- Analyze the whole PC unless the user asks for a specific unit or subsystem.\n- Do not run repair commands automatically.\n- Final answers should include likely cause, evidence from logs/status, and safe commands the user can review.\n\nUser request: {prompt}\n\nReturn action=call_tool with tool_name=collect_linux_pc_diagnostics before answering. Put arguments in scope, unit, and lines. If you use tool_input for compatibility, it must be a JSON string, not an object."
    );

    tx.send(ToolEvent::Trace(
        "before_llm_call: ask StreamRespondGuided for diagnostics action".to_string(),
    ))?;
    let (content, tool_calls) = stream_guided_response(
        &session_handle,
        &loop_prompt,
        fields.clone(),
        tools.clone(),
        options.clone(),
    )?;
    let mut response = if content.trim().is_empty() && !tool_calls.is_empty() {
        GuidedDiagnosticsLoopResponse {
            action: "call_tool".to_string(),
            tool_name: "collect_linux_pc_diagnostics".to_string(),
            scope: String::new(),
            unit: String::new(),
            lines: None,
            tool_input: String::new(),
            answer: String::new(),
        }
    } else {
        parse_guided_diagnostics_loop_response(&content, false)?
    };
    tx.send(ToolEvent::Trace(format!(
        "after_llm_call: action={}, native_tool_calls={}, tool_name={}, answer={:?}",
        response.action,
        tool_calls.len(),
        response.tool_name,
        response.answer
    )))?;

    if response.action != "call_tool" && tool_calls.is_empty() {
        tx.send(ToolEvent::Trace(
            "after_llm_call: diagnostics demo requires local evidence, running read-only tool"
                .to_string(),
        ))?;
        response.action = "call_tool".to_string();
        response.tool_name = "collect_linux_pc_diagnostics".to_string();
    }

    let result_json = if tool_calls.is_empty() {
        let arguments_json = diagnostics_arguments_from_response(&response);
        tx.send(ToolEvent::Trace(format!(
            "before_tool_execution: collect_linux_pc_diagnostics args={arguments_json}; awaiting user approval"
        )))?;
        if !request_tool_confirmation(&tx, "collect_linux_pc_diagnostics", &arguments_json)? {
            return cancel_tool_execution(&tx, &session_handle, "collect_linux_pc_diagnostics");
        }
        tx.send(ToolEvent::Trace(
            "before_tool_execution: user approved collect_linux_pc_diagnostics".to_string(),
        ))?;
        let result_json = execute_linux_pc_diagnostics_tool(&arguments_json)?;
        tx.send(ToolEvent::Trace(format!(
            "after_tool_execution: result={}",
            format_diagnostics_tool_result_for_trace(&result_json)
        )))?;
        result_json
    } else {
        let mut last_result = serde_json::Value::Null;
        for call in tool_calls {
            tx.send(ToolEvent::Trace(format!(
                "before_tool_execution: {} id={} args={}; awaiting user approval",
                call.name, call.id, call.arguments_json
            )))?;
            if call.name != "collect_linux_pc_diagnostics" {
                anyhow::bail!("unexpected diagnostics tool call: {}", call.name);
            }
            if !request_tool_confirmation(&tx, &call.name, &call.arguments_json)? {
                return cancel_tool_execution(&tx, &session_handle, &call.name);
            }
            tx.send(ToolEvent::Trace(format!(
                "before_tool_execution: user approved {} id={}",
                call.name, call.id
            )))?;
            let result_json = execute_linux_pc_diagnostics_tool(&call.arguments_json)?;
            tx.send(ToolEvent::Trace(format!(
                "after_tool_execution: result={}",
                format_diagnostics_tool_result_for_trace(&result_json)
            )))?;
            last_result = result_json;
        }
        last_result
    };
    let model_result_json = compact_diagnostics_result_for_model(&result_json);

    tx.send(ToolEvent::Trace(
        "before_llm_call: append compact diagnostics evidence and request final guidance"
            .to_string(),
    ))?;
    let final_prompt = format!(
        "User request: {prompt}\n\nRead-only diagnostics evidence follows. No changes were applied. Give a concise plain-text diagnosis with likely cause, evidence, and safe bugfix commands to review. Do not return JSON.\n\n{}",
        model_result_json
    );
    let final_answer = stream_language_text(&session_handle, &final_prompt, options, None)?;
    tx.send(ToolEvent::Trace(format!(
        "after_llm_call: final plain-text answer={:?}",
        final_answer
    )))?;
    tx.send(ToolEvent::Trace(
        "after_agent_loop: stop after one diagnostics tool round for this demo".to_string(),
    ))?;
    tx.send(ToolEvent::Final(
        if final_answer.trim().is_empty() || final_answer.trim() == "stub" {
            format_linux_pc_diagnostics_answer(&result_json)
        } else {
            final_answer
        },
    ))?;

    close_public_session(&session_handle)?;
    tx.send(ToolEvent::Done)?;
    Ok(())
}

fn execute_count_tool(prompt: &str, arguments_json: &str) -> anyhow::Result<serde_json::Value> {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments_json).unwrap_or_default();
    let word = parsed
        .get("word")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| infer_word_from_prompt(prompt))
        .unwrap_or_default();
    let character = parsed
        .get("character")
        .and_then(|value| value.as_str())
        .and_then(|value| value.chars().next())
        .or_else(|| infer_character_from_prompt(prompt))
        .unwrap_or('r');
    if word.is_empty() {
        anyhow::bail!("tool arguments did not include a word and the prompt did not contain one");
    }
    let count = word.chars().filter(|ch| *ch == character).count();
    Ok(serde_json::json!({
        "word": word,
        "character": character.to_string(),
        "count": count
    }))
}

fn execute_linux_pc_diagnostics_tool(arguments_json: &str) -> anyhow::Result<serde_json::Value> {
    let plan = build_linux_pc_diagnostics_plan(arguments_json);
    let command_results = plan
        .commands
        .iter()
        .map(run_diagnostic_command)
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "tool": "collect_linux_pc_diagnostics",
        "read_only": true,
        "scope": plan.scope,
        "unit": plan.unit,
        "lines": plan.lines,
        "fix_policy": "No changes were applied. Recommend commands only after reviewing evidence.",
        "commands": command_results,
    }))
}

fn compact_diagnostics_result_for_model(result: &serde_json::Value) -> serde_json::Value {
    let commands = result
        .get("commands")
        .and_then(|value| value.as_array())
        .map(|commands| {
            commands
                .iter()
                .map(compact_diagnostic_command_for_model)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    serde_json::json!({
        "tool": result.get("tool").cloned().unwrap_or(serde_json::Value::Null),
        "read_only": result.get("read_only").cloned().unwrap_or(serde_json::Value::Bool(true)),
        "scope": result.get("scope").cloned().unwrap_or(serde_json::Value::Null),
        "unit": result.get("unit").cloned().unwrap_or(serde_json::Value::Null),
        "lines": result.get("lines").cloned().unwrap_or(serde_json::Value::Null),
        "fix_policy": result.get("fix_policy").cloned().unwrap_or(serde_json::Value::Null),
        "commands": commands,
    })
}

fn compact_diagnostic_command_for_model(command: &serde_json::Value) -> serde_json::Value {
    let mut compact = serde_json::Map::new();
    for key in ["label", "command", "success", "status_code", "error"] {
        if let Some(value) = command.get(key) {
            compact.insert(key.to_string(), value.clone());
        }
    }
    if let Some(stdout) = command.get("stdout").and_then(|value| value.as_str())
        && !stdout.trim().is_empty()
    {
        compact.insert(
            "stdout_excerpt".to_string(),
            serde_json::Value::String(truncate_text(stdout, 600)),
        );
    }
    if let Some(stderr) = command.get("stderr").and_then(|value| value.as_str())
        && !stderr.trim().is_empty()
    {
        compact.insert(
            "stderr_excerpt".to_string(),
            serde_json::Value::String(truncate_text(stderr, 300)),
        );
    }

    serde_json::Value::Object(compact)
}

fn diagnostics_arguments_from_response(response: &GuidedDiagnosticsLoopResponse) -> String {
    let mut args = serde_json::Map::new();
    if is_non_stub(&response.scope) {
        args.insert(
            "scope".to_string(),
            serde_json::Value::String(response.scope.trim().to_string()),
        );
    }
    if is_non_stub(&response.unit) {
        args.insert(
            "unit".to_string(),
            serde_json::Value::String(response.unit.trim().to_string()),
        );
    }
    if let Some(lines) = response.lines
        && lines > 0
    {
        args.insert("lines".to_string(), serde_json::Value::from(lines));
    }
    if is_non_stub(&response.tool_input) {
        if let Ok(serde_json::Value::Object(parsed_tool_input)) =
            serde_json::from_str::<serde_json::Value>(&response.tool_input)
        {
            for key in ["scope", "unit", "lines"] {
                if let Some(value) = parsed_tool_input.get(key)
                    && !args.contains_key(key)
                {
                    args.insert(key.to_string(), value.clone());
                }
            }
        } else {
            args.insert(
                "tool_input".to_string(),
                serde_json::Value::String(response.tool_input.trim().to_string()),
            );
        }
    }

    serde_json::Value::Object(args).to_string()
}

fn is_non_stub(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed != "stub"
}

fn build_linux_pc_diagnostics_plan(arguments_json: &str) -> LinuxDiagnosticsPlan {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments_json).unwrap_or_default();
    let parsed_tool_input = parse_tool_input(&parsed);
    let scope = diagnostics_arg(&parsed, parsed_tool_input.as_ref(), "scope")
        .and_then(|value| value.as_str())
        .filter(|value| matches!(*value, "all" | "user" | "system"))
        .unwrap_or("all")
        .to_string();
    let unit = diagnostics_arg(&parsed, parsed_tool_input.as_ref(), "unit")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| is_safe_systemd_unit(value))
        .map(str::to_string);
    let lines = diagnostics_arg(&parsed, parsed_tool_input.as_ref(), "lines")
        .and_then(|value| value.as_u64())
        .unwrap_or(120)
        .clamp(20, 300);

    let mut commands = vec![
        diagnostic_command("kernel and OS version", "uname", vec!["-a"]),
        diagnostic_command("uptime and load", "uptime", vec![]),
        diagnostic_command(
            "disk usage",
            "df",
            vec!["-h", "-x", "tmpfs", "-x", "devtmpfs"],
        ),
        diagnostic_command("memory and swap summary", "free", vec!["-h"]),
        diagnostic_command("swap devices", "swapon", vec!["--show"]),
    ];

    if scope == "all" || scope == "system" {
        commands.push(diagnostic_command(
            "failed system units",
            "systemctl",
            vec!["--no-pager", "--failed"],
        ));
        if let Some(unit) = &unit {
            commands.push(diagnostic_command(
                "selected system unit status",
                "systemctl",
                vec!["--no-pager", "status", unit],
            ));
            commands.push(diagnostic_command(
                "selected system unit journal",
                "journalctl",
                vec![
                    "--no-pager",
                    "--since",
                    "2 hours ago",
                    "-n",
                    &lines.to_string(),
                    "-u",
                    unit,
                ],
            ));
        }
        commands.push(diagnostic_command(
            "recent system journal warnings",
            "journalctl",
            vec![
                "--no-pager",
                "--since",
                "2 hours ago",
                "-n",
                &lines.to_string(),
                "-p",
                "warning",
            ],
        ));
        commands.push(diagnostic_command(
            "recent kernel warnings",
            "journalctl",
            vec![
                "--no-pager",
                "--since",
                "2 hours ago",
                "-n",
                &lines.to_string(),
                "-k",
                "-p",
                "warning",
            ],
        ));
    }

    if scope == "all" || scope == "user" {
        commands.push(diagnostic_command(
            "failed user units",
            "systemctl",
            vec!["--user", "--no-pager", "--failed"],
        ));
        if let Some(unit) = &unit {
            commands.push(diagnostic_command(
                "selected user unit status",
                "systemctl",
                vec!["--user", "--no-pager", "status", unit],
            ));
            commands.push(diagnostic_command(
                "selected user unit journal",
                "journalctl",
                vec![
                    "--user",
                    "--no-pager",
                    "--since",
                    "2 hours ago",
                    "-n",
                    &lines.to_string(),
                    "-u",
                    unit,
                ],
            ));
        }
        commands.push(diagnostic_command(
            "recent user journal warnings",
            "journalctl",
            vec![
                "--user",
                "--no-pager",
                "--since",
                "2 hours ago",
                "-n",
                &lines.to_string(),
                "-p",
                "warning",
            ],
        ));
    }

    LinuxDiagnosticsPlan {
        scope,
        unit,
        lines,
        commands,
    }
}

fn diagnostics_arg<'a>(
    parsed: &'a serde_json::Value,
    parsed_tool_input: Option<&'a serde_json::Value>,
    key: &str,
) -> Option<&'a serde_json::Value> {
    parsed.get(key).or_else(|| {
        parsed
            .get("tool_input")
            .and_then(|value| value.get(key))
            .or_else(|| parsed_tool_input.and_then(|value| value.get(key)))
    })
}

fn parse_tool_input(parsed: &serde_json::Value) -> Option<serde_json::Value> {
    parsed
        .get("tool_input")
        .and_then(|value| value.as_str())
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
}

fn diagnostic_command(label: &str, program: &str, args: Vec<&str>) -> DiagnosticCommand {
    DiagnosticCommand {
        label: label.to_string(),
        program: program.to_string(),
        args: args.into_iter().map(str::to_string).collect(),
    }
}

fn run_diagnostic_command(command: &DiagnosticCommand) -> serde_json::Value {
    match std::process::Command::new(&command.program)
        .args(&command.args)
        .output()
    {
        Ok(output) => serde_json::json!({
            "label": command.label,
            "command": display_diagnostic_command(command),
            "success": output.status.success(),
            "status_code": output.status.code(),
            "stdout": truncate_text(String::from_utf8_lossy(&output.stdout).as_ref(), 8_000),
            "stderr": truncate_text(String::from_utf8_lossy(&output.stderr).as_ref(), 4_000),
        }),
        Err(error) => serde_json::json!({
            "label": command.label,
            "command": display_diagnostic_command(command),
            "success": false,
            "error": error.to_string(),
        }),
    }
}

fn display_diagnostic_command(command: &DiagnosticCommand) -> String {
    let mut parts = Vec::with_capacity(command.args.len() + 1);
    parts.push(command.program.clone());
    parts.extend(command.args.clone());
    parts.join(" ")
}

fn is_safe_systemd_unit(unit: &str) -> bool {
    !unit.is_empty()
        && unit.len() <= 128
        && !unit.starts_with('-')
        && unit.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '@' | ':' | '\\')
        })
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= max_chars {
            out.push_str("\n...[truncated]");
            return out;
        }
        out.push(ch);
    }
    out
}

fn guided_tool_loop_fields() -> Vec<(String, String, String, bool)> {
    vec![
        (
            "action".to_string(),
            "string".to_string(),
            "Either call_tool or final".to_string(),
            true,
        ),
        (
            "tool_name".to_string(),
            "string".to_string(),
            "Tool to execute, usually count_character_occurrences, or empty for final".to_string(),
            true,
        ),
        (
            "word".to_string(),
            "string".to_string(),
            "Word or text to pass to the tool".to_string(),
            true,
        ),
        (
            "character".to_string(),
            "string".to_string(),
            "Single character to pass to the tool".to_string(),
            true,
        ),
        (
            "answer".to_string(),
            "string".to_string(),
            "Final user-facing answer when action is final, otherwise empty".to_string(),
            false,
        ),
    ]
}

fn count_tool_definitions() -> anyhow::Result<Vec<ToolDefinitionDbus>> {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["word", "character"],
        "properties": {
            "word": {"type": "string", "description": "The word or short text to inspect"},
            "character": {"type": "string", "description": "The single character to count"}
        },
        "additionalProperties": false
    });

    Ok(vec![ToolDefinitionDbus {
        name: "count_character_occurrences".to_string(),
        description: "Count how many times one character appears in a word or short text."
            .to_string(),
        schema_json: serde_json::to_string(&schema)?,
    }])
}

fn guided_linux_pc_diagnostics_loop_fields() -> Vec<(String, String, String, bool)> {
    vec![
        (
            "action".to_string(),
            "string".to_string(),
            "Either call_tool or final".to_string(),
            true,
        ),
        (
            "tool_name".to_string(),
            "string".to_string(),
            "Tool to execute, usually collect_linux_pc_diagnostics, or empty for final".to_string(),
            true,
        ),
        (
            "scope".to_string(),
            "string".to_string(),
            "Optional diagnostics scope: all, user, or system".to_string(),
            false,
        ),
        (
            "unit".to_string(),
            "string".to_string(),
            "Optional systemd unit to inspect".to_string(),
            false,
        ),
        (
            "lines".to_string(),
            "integer".to_string(),
            "Optional journal line limit".to_string(),
            false,
        ),
        (
            "tool_input".to_string(),
            "string".to_string(),
            "Optional JSON string containing scope, unit, and lines for compatibility with tool-call-shaped model output".to_string(),
            false,
        ),
        (
            "answer".to_string(),
            "string".to_string(),
            "Final user-facing diagnosis and safe bugfix guidance when action is final".to_string(),
            false,
        ),
    ]
}

fn linux_pc_diagnostics_tool_definitions() -> anyhow::Result<Vec<ToolDefinitionDbus>> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "scope": {
                "type": "string",
                "enum": ["all", "user", "system"],
                "description": "Read all PC diagnostics, user-session diagnostics, or system diagnostics; all is the default"
            },
            "unit": {
                "type": "string",
                "description": "Optional systemd unit to inspect when a service-specific issue is suspected"
            },
            "lines": {
                "type": "integer",
                "description": "Maximum recent journal lines to collect, clamped to 20..300"
            }
        },
        "additionalProperties": false
    });

    Ok(vec![ToolDefinitionDbus {
        name: "collect_linux_pc_diagnostics".to_string(),
        description: "Collect bounded read-only Linux PC status, resource usage, failed units, kernel messages, and journal excerpts from the local machine."
            .to_string(),
        schema_json: serde_json::to_string(&schema)?,
    }])
}

fn parse_guided_tool_loop_response(
    content: &str,
    prompt: &str,
    expect_final: bool,
) -> anyhow::Result<GuidedToolLoopResponse> {
    let mut response = serde_json::from_str::<GuidedToolLoopResponse>(content)?;
    if expect_final {
        response.action = "final".to_string();
    }
    if response.action != "call_tool" && response.action != "final" {
        response.action = if content.contains("tool_result") {
            "final".to_string()
        } else {
            "call_tool".to_string()
        };
    }
    if response.tool_name.trim().is_empty() || response.tool_name == "stub" {
        response.tool_name = "count_character_occurrences".to_string();
    }
    if response.word.trim().is_empty() || response.word == "stub" {
        response.word = infer_word_from_prompt(prompt).unwrap_or_default();
    }
    if response.character.trim().is_empty() || response.character == "stub" {
        response.character = infer_character_from_prompt(prompt)
            .unwrap_or('r')
            .to_string();
    }
    Ok(response)
}

fn parse_guided_diagnostics_loop_response(
    content: &str,
    expect_final: bool,
) -> anyhow::Result<GuidedDiagnosticsLoopResponse> {
    let mut response = serde_json::from_str::<GuidedDiagnosticsLoopResponse>(content)?;
    if expect_final {
        response.action = "final".to_string();
    }
    if response.action != "call_tool" && response.action != "final" {
        response.action = if content.contains("tool_result") {
            "final".to_string()
        } else {
            "call_tool".to_string()
        };
    }
    if response.tool_name.trim().is_empty() || response.tool_name == "stub" {
        response.tool_name = "collect_linux_pc_diagnostics".to_string();
    }
    Ok(response)
}

fn format_tool_result_answer(result: &serde_json::Value) -> String {
    let word = result["word"].as_str().unwrap_or("the input");
    let character = result["character"].as_str().unwrap_or("?");
    let count = result["count"].as_u64().unwrap_or_default();
    format!("The character '{character}' occurs {count} times in {word}.")
}

fn format_linux_pc_diagnostics_answer(result: &serde_json::Value) -> String {
    let failed_commands = result
        .get("commands")
        .and_then(|value| value.as_array())
        .map(|commands| {
            commands
                .iter()
                .filter(|command| {
                    !command
                        .get("success")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or_default();

    if failed_commands == 0 {
        "Collected read-only Linux PC diagnostics from the local machine. No collection command reported failure. Review the status, resource, kernel, and journal evidence in the trace, then apply only the specific fix that matches the evidence; no changes were applied by this demo."
            .to_string()
    } else {
        format!(
            "Collected read-only Linux PC diagnostics from the local machine, but {failed_commands} collection command(s) returned an error. Check the trace for permission or missing-command details, then rerun the relevant command manually before applying a fix. No changes were applied by this demo."
        )
    }
}

fn format_diagnostics_tool_result_for_trace(result: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string());
    truncate_text(&pretty, 20_000)
}

fn initial_final_answer(response: GuidedToolLoopResponse) -> anyhow::Result<String> {
    let answer = response.answer.trim().to_string();
    if answer.is_empty() || answer == "stub" {
        anyhow::bail!("guided response selected final without an answer");
    }
    Ok(answer)
}

fn infer_word_from_prompt(prompt: &str) -> Option<String> {
    let parts = prompt
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| part.len() > 1)
        .collect::<Vec<_>>();

    parts
        .iter()
        .enumerate()
        .rev()
        .find(|(_, part)| part.eq_ignore_ascii_case("in") || part.eq_ignore_ascii_case("within"))
        .and_then(|(index, _)| {
            parts[index + 1..]
                .iter()
                .rev()
                .find(|part| is_count_target_candidate(part))
        })
        .or_else(|| {
            parts
                .iter()
                .rev()
                .find(|part| is_count_target_candidate(part))
        })
        .map(|part| (*part).to_string())
}

fn is_count_target_candidate(part: &str) -> bool {
    let lower = part.to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "a" | "an"
            | "are"
            | "character"
            | "count"
            | "does"
            | "how"
            | "in"
            | "input"
            | "is"
            | "letter"
            | "many"
            | "occur"
            | "occurs"
            | "of"
            | "string"
            | "text"
            | "the"
            | "times"
            | "to"
            | "within"
            | "word"
    )
}

fn infer_character_from_prompt(prompt: &str) -> Option<char> {
    let lower = prompt.to_ascii_lowercase();
    lower
        .split("letter")
        .nth(1)
        .and_then(|rest| rest.chars().find(|ch| ch.is_ascii_alphabetic()))
        .or_else(|| {
            lower
                .split("character")
                .nth(1)
                .and_then(|rest| rest.chars().find(|ch| ch.is_ascii_alphabetic()))
        })
        .or_else(|| {
            lower
                .split(|ch: char| !ch.is_ascii_alphanumeric())
                .find_map(|part| {
                    let mut chars = part.chars();
                    let ch = chars.next()?;
                    (chars.next().is_none() && ch.is_ascii_alphabetic()).then_some(ch)
                })
        })
}

#[cfg(test)]
mod tests {
    use super::{
        build_linux_pc_diagnostics_plan, compact_diagnostics_result_for_model,
        diagnostics_arguments_from_response, execute_count_tool,
        guided_linux_pc_diagnostics_loop_fields, guided_tool_loop_fields, initial_final_answer,
        is_safe_systemd_unit, parse_guided_diagnostics_loop_response,
        parse_guided_tool_loop_response,
    };
    use hegel::TestCase;
    use hegel::generators as gs;

    #[test]
    fn count_tool_uses_structured_arguments() {
        let result = execute_count_tool(
            "ignored prompt",
            r#"{"word":"strawrberrry","character":"r"}"#,
        )
        .expect("count tool should run");

        assert_eq!(result["count"], 5);
    }

    #[hegel::test]
    fn count_tool_counts_generated_structured_arguments(tc: TestCase) {
        let mut chars =
            tc.draw(gs::vecs(gs::sampled_from(vec!['a', 'b', 'e', 'r', 'z'])).max_size(32));
        if chars.is_empty() {
            chars.push('r');
        }
        let word = chars.iter().collect::<String>();
        let character = tc.draw(gs::sampled_from(vec!['a', 'b', 'e', 'r', 'z']));
        let arguments = serde_json::json!({
            "word": word,
            "character": character.to_string(),
        })
        .to_string();

        let result =
            execute_count_tool("ignored prompt", &arguments).expect("count tool should run");
        let expected = word.chars().filter(|ch| *ch == character).count() as u64;

        assert_eq!(result["word"], word);
        assert_eq!(result["character"], character.to_string());
        assert_eq!(result["count"].as_u64(), Some(expected));
    }

    #[test]
    fn count_tool_falls_back_to_prompt_for_stub_arguments() {
        let result = execute_count_tool(
            "How many times does the letter r occur in strawrberrry?",
            "{}",
        )
        .expect("count tool should infer demo args");

        assert_eq!(result["word"], "strawrberrry");
        assert_eq!(result["character"], "r");
        assert_eq!(result["count"], 5);
    }

    #[test]
    fn count_tool_infers_non_r_prompt_arguments() {
        let result = execute_count_tool(
            "How many times does the letter s occur in Mississippi?",
            "{}",
        )
        .expect("count tool should infer non-r demo args");

        assert_eq!(result["word"], "Mississippi");
        assert_eq!(result["character"], "s");
        assert_eq!(result["count"], 4);
    }

    #[hegel::test]
    fn guided_tool_loop_stub_fields_are_inferred_from_prompt(tc: TestCase) {
        let word = tc.draw(gs::sampled_from(vec![
            "strawberry".to_string(),
            "raspberry".to_string(),
            "cranberry".to_string(),
        ]));
        let character = tc.draw(gs::sampled_from(vec!['r', 'e', 'a']));
        let prompt = format!("How many times does the letter {character} occur in {word}?");
        let response = parse_guided_tool_loop_response(
            r#"{"action":"call_tool","tool_name":"stub","word":"stub","character":"stub","answer":""}"#,
            &prompt,
            false,
        )
        .expect("stub response should be repaired");

        assert_eq!(response.action, "call_tool");
        assert_eq!(response.tool_name, "count_character_occurrences");
        assert_eq!(response.word, word);
        assert_eq!(response.character, character.to_string());
    }

    #[test]
    fn guided_tool_loop_rejects_missing_text_tool_arguments() {
        let error = parse_guided_tool_loop_response(
            r#"{"action":"call_tool","tool_name":"count_character_occurrences","answer":""}"#,
            "How many times does the letter s occur in Mississippi?",
            false,
        )
        .expect_err("text tool calls should include structured arguments");

        assert!(error.to_string().contains("missing field"));
    }

    #[test]
    fn guided_tool_loop_schema_requires_tool_args_but_not_answer() {
        let required_fields = guided_tool_loop_fields()
            .into_iter()
            .filter_map(|(name, _, _, required)| required.then_some(name))
            .collect::<Vec<_>>();

        assert_eq!(
            required_fields,
            vec!["action", "tool_name", "word", "character"]
        );
    }

    #[test]
    fn diagnostics_plan_defaults_to_read_only_whole_pc_checks() {
        let plan = build_linux_pc_diagnostics_plan("{}");

        assert_eq!(plan.scope, "all");
        assert_eq!(plan.lines, 120);
        assert!(plan.unit.is_none());
        assert!(plan.commands.iter().any(|command| command.program == "df"));
        assert!(
            plan.commands
                .iter()
                .any(|command| command.program == "free")
        );
        assert!(
            plan.commands
                .iter()
                .any(|command| command.program == "journalctl"
                    && command.args.iter().any(|arg| arg == "--user"))
        );
        assert!(
            plan.commands
                .iter()
                .any(|command| command.program == "journalctl"
                    && command.args.iter().any(|arg| arg == "-k"))
        );
        assert!(
            plan.commands
                .iter()
                .any(|command| command.program == "systemctl"
                    && command.args.iter().any(|arg| arg == "--failed"))
        );
    }

    #[test]
    fn diagnostics_plan_clamps_lines_and_rejects_unsafe_units() {
        let plan = build_linux_pc_diagnostics_plan(
            r#"{"scope":"system","unit":"../../bad;unit","lines":9999}"#,
        );

        assert_eq!(plan.scope, "system");
        assert_eq!(plan.lines, 300);
        assert!(plan.unit.is_none());
        assert!(plan.commands.iter().all(|command| {
            !command
                .args
                .iter()
                .any(|arg| arg.contains("../../bad;unit"))
        }));
    }

    #[test]
    fn diagnostics_plan_accepts_tool_input_object_wrapper() {
        let plan = build_linux_pc_diagnostics_plan(
            r#"{"tool_input":{"scope":"user","unit":"app@example.service","lines":40}}"#,
        );

        assert_eq!(plan.scope, "user");
        assert_eq!(plan.unit.as_deref(), Some("app@example.service"));
        assert_eq!(plan.lines, 40);
    }

    #[test]
    fn diagnostics_response_maps_tool_input_json_string_to_arguments() {
        let response = parse_guided_diagnostics_loop_response(
            r#"{"action":"call_tool","tool_name":"collect_linux_pc_diagnostics","tool_input":"{\"scope\":\"system\",\"lines\":60}","answer":""}"#,
            false,
        )
        .expect("tool_input JSON string should parse");

        let arguments = serde_json::from_str::<serde_json::Value>(
            &diagnostics_arguments_from_response(&response),
        )
        .expect("arguments should be JSON");

        assert_eq!(arguments["scope"], "system");
        assert_eq!(arguments["lines"], 60);
    }

    #[test]
    fn diagnostics_schema_requires_action_and_tool_name_only() {
        let required_fields = guided_linux_pc_diagnostics_loop_fields()
            .into_iter()
            .filter_map(|(name, _, _, required)| required.then_some(name))
            .collect::<Vec<_>>();

        assert_eq!(required_fields, vec!["action", "tool_name"]);
    }

    #[test]
    fn diagnostics_schema_allows_tool_input_compatibility_field() {
        let tool_input_kind = guided_linux_pc_diagnostics_loop_fields()
            .into_iter()
            .find_map(|(name, kind, _, _)| (name == "tool_input").then_some(kind));

        assert_eq!(tool_input_kind.as_deref(), Some("string"));
    }

    #[test]
    fn diagnostics_model_payload_compacts_long_command_output() {
        let result = serde_json::json!({
            "tool": "collect_linux_pc_diagnostics",
            "read_only": true,
            "scope": "all",
            "unit": null,
            "lines": 120,
            "fix_policy": "No changes were applied.",
            "commands": [{
                "label": "recent system journal warnings",
                "command": "journalctl --no-pager -p warning",
                "success": true,
                "status_code": 0,
                "stdout": "x".repeat(5_000),
                "stderr": "y".repeat(1_000),
            }],
        });

        let compact = compact_diagnostics_result_for_model(&result);
        let command = &compact["commands"][0];

        assert!(command.get("stdout").is_none());
        assert!(command["stdout_excerpt"].as_str().unwrap().len() < 700);
        assert!(command["stderr_excerpt"].as_str().unwrap().len() < 400);
    }

    #[test]
    fn diagnostics_unit_validation_allows_normal_systemd_units() {
        assert!(is_safe_systemd_unit("aileron-daemon.service"));
        assert!(is_safe_systemd_unit("app@example.service"));
        assert!(!is_safe_systemd_unit("--help"));
        assert!(!is_safe_systemd_unit("bad;unit.service"));
    }

    #[test]
    fn initial_final_answer_rejects_empty_answer() {
        let response = parse_guided_tool_loop_response(
            r#"{"action":"final","tool_name":"","word":"","character":""}"#,
            "What time is it?",
            false,
        )
        .expect("answer is optional in the schema");

        let error = initial_final_answer(response).expect_err("empty final answer should fail");

        assert!(error.to_string().contains("without an answer"));
    }
}
