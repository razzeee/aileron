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
    MultiToolChoice,
    LinuxDiagnostics,
}

impl ToolDemoCase {
    pub(crate) fn labels() -> [&'static str; 3] {
        [
            ToolDemoCase::CharacterCounter.label(),
            ToolDemoCase::MultiToolChoice.label(),
            ToolDemoCase::LinuxDiagnostics.label(),
        ]
    }

    pub(crate) fn index(&self) -> u32 {
        match self {
            ToolDemoCase::CharacterCounter => 0,
            ToolDemoCase::MultiToolChoice => 1,
            ToolDemoCase::LinuxDiagnostics => 2,
        }
    }

    pub(crate) fn from_index(index: u32) -> Option<Self> {
        match index {
            0 => Some(ToolDemoCase::CharacterCounter),
            1 => Some(ToolDemoCase::MultiToolChoice),
            2 => Some(ToolDemoCase::LinuxDiagnostics),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            ToolDemoCase::CharacterCounter => "Character counter",
            ToolDemoCase::MultiToolChoice => "Native multi-tool choice",
            ToolDemoCase::LinuxDiagnostics => "Linux PC diagnostics",
        }
    }

    pub(crate) fn default_prompt(&self) -> &'static str {
        match self {
            ToolDemoCase::CharacterCounter => {
                "How many times does the letter r occur in strawrberrry?"
            }
            ToolDemoCase::MultiToolChoice => {
                "Use count_character_occurrences to count the letter s in Mississippi. Do not run diagnostics."
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
            ToolDemoCase::MultiToolChoice => {
                "Offer multiple app tools and verify the model chooses one specific tool call."
            }
            ToolDemoCase::LinuxDiagnostics => {
                "Collect read-only Linux PC diagnostics locally, then ask the model for fix guidance."
            }
        }
    }

    pub(crate) fn running_detail(&self) -> &'static str {
        match self {
            ToolDemoCase::CharacterCounter => "The app owns the loop and executes tools locally.",
            ToolDemoCase::MultiToolChoice => {
                "The app offered multiple tools and is waiting for one concrete tool call."
            }
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

#[derive(Debug, Clone, Deserialize)]
struct GuidedMultiToolChoiceResponse {
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    word: String,
    #[serde(default)]
    character: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    unit: String,
    #[serde(default)]
    lines: Option<i64>,
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
        ToolDemoCase::MultiToolChoice => run_multi_tool_choice_demo(prompt, tx),
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

fn run_multi_tool_choice_demo(
    prompt: &str,
    tx: std::sync::mpsc::Sender<ToolEvent>,
) -> anyhow::Result<()> {
    tx.send(ToolEvent::Trace(
        "before_agent_loop: register count_character_occurrences and collect_linux_pc_diagnostics"
            .to_string(),
    ))?;

    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let session_handle = create_public_session(
        &proxy,
        "language.analyze",
        "You are a local tool router. Choose exactly one app-provided tool that matches the user request. Do not call every available tool.",
    )?;

    let fields = guided_multi_tool_choice_fields();
    let tools = multi_tool_choice_definitions()?;
    let options = generation_options(256, "", "");
    let loop_prompt = format!(
        "Available app tools include count_character_occurrences plus individual read-only Linux diagnostics tools such as get_disk_usage, get_memory_summary, get_failed_system_units, and get_recent_kernel_warnings.\n\nUser request: {prompt}\n\nChoose one concrete tool. Prefer count_character_occurrences for counting text. Prefer a specific diagnostics tool only for Linux PC status, logs, resources, failed units, or bugfix evidence. If native tool calls are unavailable, return JSON with tool_name and arguments for the one selected tool."
    );

    tx.send(ToolEvent::Trace(
        "before_llm_call: ask StreamRespondGuided with multiple available tools".to_string(),
    ))?;
    let (content, tool_calls) = stream_guided_response(
        &session_handle,
        &loop_prompt,
        fields.clone(),
        tools.clone(),
        options.clone(),
    )?;
    tx.send(ToolEvent::Trace(format!(
        "after_llm_call: native_tool_calls={}, fallback_json={:?}",
        tool_calls.len(),
        content
    )))?;

    let mut results = Vec::new();
    let (tool_name, result_json) = if tool_calls.is_empty() {
        let response = parse_guided_multi_tool_choice_response(&content, prompt)?;
        let tool_name = response.tool_name.clone();
        let arguments_json = multi_tool_arguments_from_response(&response);
        tx.send(ToolEvent::Trace(format!(
            "before_tool_execution: {tool_name} args={arguments_json}; awaiting user approval"
        )))?;
        if !request_tool_confirmation(&tx, &tool_name, &arguments_json)? {
            return cancel_tool_execution(&tx, &session_handle, &tool_name);
        }
        tx.send(ToolEvent::Trace(format!(
            "before_tool_execution: user approved {tool_name}"
        )))?;
        let result_json = execute_multi_tool_call(prompt, &tool_name, &arguments_json)?;
        tx.send(ToolEvent::Trace(format!(
            "after_tool_execution: result={}",
            format_multi_tool_result_for_trace(&tool_name, &result_json)
        )))?;
        (tool_name, result_json)
    } else {
        let mut last_tool_name = String::new();
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
            let result_json = execute_multi_tool_call(prompt, &call.name, &call.arguments_json)?;
            tx.send(ToolEvent::Trace(format!(
                "after_tool_execution: result={}",
                format_multi_tool_result_for_trace(&call.name, &result_json)
            )))?;
            results.push(ToolResultDbus {
                id: call.id,
                content: result_json.to_string(),
                content_json: result_json.to_string(),
            });
            last_tool_name = call.name;
            last_result = result_json;
        }
        (last_tool_name, last_result)
    };

    tx.send(ToolEvent::Trace(
        "before_llm_call: submit the selected tool result and request final answer".to_string(),
    ))?;
    let final_prompt = format!(
        "User request: {prompt}\n\nThe app executed exactly one selected tool: {tool_name}. Tool result:\n{result_json}\n\nReturn JSON with answer."
    );
    let final_content = if results.is_empty() {
        let (content, _) =
            stream_guided_response(&session_handle, &final_prompt, fields, Vec::new(), options)?;
        content
    } else {
        let (content, _) = stream_guided_tool_results(
            &session_handle,
            &final_prompt,
            results,
            fields,
            tools,
            options,
        )?;
        content
    };
    tx.send(ToolEvent::Trace(format!(
        "after_llm_call: final_json={final_content:?}"
    )))?;
    let final_response = parse_guided_multi_tool_choice_response(&final_content, prompt).ok();
    tx.send(ToolEvent::Final(
        final_response
            .and_then(|response| (!response.answer.trim().is_empty()).then_some(response.answer))
            .unwrap_or_else(|| format_multi_tool_result_answer(&tool_name, &result_json)),
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
        "before_agent_loop: seed messages and register individual read-only diagnostics tools"
            .to_string(),
    ))?;

    let conn = portal_connection()?;
    let proxy = zbus::blocking::Proxy::new(&conn, PORTAL_BUS, PORTAL_PATH, LANGUAGE_IFACE)?;
    let session_handle = create_public_session(
        &proxy,
        "language.analyze",
        "You are a local Linux PC diagnostics assistant. Choose only the app-provided read-only diagnostics tools needed for the user's question before answering. Recommend safe bugfix steps, but do not claim that you changed the machine.",
    )?;

    let fields = guided_linux_pc_diagnostics_loop_fields();
    let tools = linux_pc_diagnostics_tool_definitions()?;
    let options = generation_options(384, "", "");
    let loop_prompt = format!(
        "Available read-only app tools:\n- get_kernel_os_version(): uname -a.\n- get_uptime_load(): uptime.\n- get_disk_usage(): df excluding tmpfs/devtmpfs.\n- get_memory_summary(): free -h.\n- get_swap_devices(): swapon --show.\n- get_failed_system_units(): systemctl --failed.\n- get_system_unit_status(unit): systemctl status for a safe unit name.\n- get_system_unit_journal(unit, lines): recent journal for a safe system unit.\n- get_recent_system_warnings(lines): recent system warning journal.\n- get_recent_kernel_warnings(lines): recent kernel warning journal.\n- get_failed_user_units(): systemctl --user --failed.\n- get_user_unit_status(unit): user systemctl status for a safe unit name.\n- get_user_unit_journal(unit, lines): recent user journal for a safe unit.\n- get_recent_user_warnings(lines): recent user warning journal.\n\nPolicy:\n- The app may read local status and logs only.\n- Choose only the specific tools needed for the user's question; do not run every tool by default.\n- Do not run repair commands automatically.\n- Final answers should include likely cause, evidence from logs/status, and safe commands the user can review.\n\nUser request: {prompt}\n\nPrefer native tool calls and call one or more concrete tools. If native tool calls are unavailable, return JSON with action=call_tool, one tool_name, and optional unit/lines."
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
        let mut command_results = Vec::new();
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
            let result_json =
                execute_linux_diagnostics_tool_call(&call.name, &call.arguments_json)?;
            tx.send(ToolEvent::Trace(format!(
                "after_tool_execution: result={}",
                format_diagnostics_tool_result_for_trace(&result_json)
            )))?;
            command_results.extend(
                result_json["commands"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        linux_pc_diagnostics_result("selected_diagnostics_tools", None, 120, command_results)
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

    Ok(linux_pc_diagnostics_result(
        &plan.scope,
        plan.unit.as_deref(),
        plan.lines,
        command_results,
    ))
}

fn execute_linux_diagnostics_tool_call(
    tool_name: &str,
    arguments_json: &str,
) -> anyhow::Result<serde_json::Value> {
    let parsed = serde_json::from_str::<serde_json::Value>(arguments_json).unwrap_or_default();
    let command = diagnostic_command_for_tool(tool_name, &parsed)?;
    let command_result = run_diagnostic_command(&command);
    Ok(linux_pc_diagnostics_result(
        tool_name,
        parsed.get("unit").and_then(|value| value.as_str()),
        diagnostics_lines_arg(&parsed),
        vec![command_result],
    ))
}

fn linux_pc_diagnostics_result(
    scope: &str,
    unit: Option<&str>,
    lines: u64,
    command_results: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "tool": "collect_linux_pc_diagnostics",
        "read_only": true,
        "scope": scope,
        "unit": unit,
        "lines": lines,
        "fix_policy": "No changes were applied. Recommend commands only after reviewing evidence.",
        "commands": command_results,
    })
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

fn diagnostic_command_for_tool(
    tool_name: &str,
    parsed: &serde_json::Value,
) -> anyhow::Result<DiagnosticCommand> {
    let lines = diagnostics_lines_arg(parsed);
    let unit = parsed
        .get("unit")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| is_safe_systemd_unit(value));

    Ok(match tool_name {
        "get_kernel_os_version" => diagnostic_command("kernel and OS version", "uname", vec!["-a"]),
        "get_uptime_load" => diagnostic_command("uptime and load", "uptime", vec![]),
        "get_disk_usage" => diagnostic_command(
            "disk usage",
            "df",
            vec!["-h", "-x", "tmpfs", "-x", "devtmpfs"],
        ),
        "get_memory_summary" => diagnostic_command("memory and swap summary", "free", vec!["-h"]),
        "get_swap_devices" => diagnostic_command("swap devices", "swapon", vec!["--show"]),
        "get_failed_system_units" => diagnostic_command(
            "failed system units",
            "systemctl",
            vec!["--no-pager", "--failed"],
        ),
        "get_system_unit_status" => {
            let unit = unit.ok_or_else(|| anyhow::anyhow!("missing or unsafe system unit"))?;
            diagnostic_command(
                "selected system unit status",
                "systemctl",
                vec!["--no-pager", "status", unit],
            )
        }
        "get_system_unit_journal" => {
            let unit = unit.ok_or_else(|| anyhow::anyhow!("missing or unsafe system unit"))?;
            diagnostic_command(
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
            )
        }
        "get_recent_system_warnings" => diagnostic_command(
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
        ),
        "get_recent_kernel_warnings" => diagnostic_command(
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
        ),
        "get_failed_user_units" => diagnostic_command(
            "failed user units",
            "systemctl",
            vec!["--user", "--no-pager", "--failed"],
        ),
        "get_user_unit_status" => {
            let unit = unit.ok_or_else(|| anyhow::anyhow!("missing or unsafe user unit"))?;
            diagnostic_command(
                "selected user unit status",
                "systemctl",
                vec!["--user", "--no-pager", "status", unit],
            )
        }
        "get_user_unit_journal" => {
            let unit = unit.ok_or_else(|| anyhow::anyhow!("missing or unsafe user unit"))?;
            diagnostic_command(
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
            )
        }
        "get_recent_user_warnings" => diagnostic_command(
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
        ),
        _ => anyhow::bail!("unexpected diagnostics tool call: {tool_name}"),
    })
}

fn diagnostics_lines_arg(parsed: &serde_json::Value) -> u64 {
    parsed
        .get("lines")
        .and_then(|value| value.as_u64())
        .unwrap_or(120)
        .clamp(20, 300)
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

fn multi_tool_choice_definitions() -> anyhow::Result<Vec<ToolDefinitionDbus>> {
    let mut tools = count_tool_definitions()?;
    tools.extend(linux_pc_diagnostics_tool_definitions()?);
    Ok(tools)
}

fn guided_multi_tool_choice_fields() -> Vec<(String, String, String, bool)> {
    vec![
        (
            "tool_name".to_string(),
            "string".to_string(),
            "Exactly one selected tool, such as count_character_occurrences or a get_* diagnostics tool".to_string(),
            true,
        ),
        (
            "word".to_string(),
            "string".to_string(),
            "Word or text for count_character_occurrences".to_string(),
            false,
        ),
        (
            "character".to_string(),
            "string".to_string(),
            "Single character for count_character_occurrences".to_string(),
            false,
        ),
        (
            "scope".to_string(),
            "string".to_string(),
            "Diagnostics scope: all, user, or system".to_string(),
            false,
        ),
        (
            "unit".to_string(),
            "string".to_string(),
            "Optional systemd unit for diagnostics".to_string(),
            false,
        ),
        (
            "lines".to_string(),
            "integer".to_string(),
            "Optional diagnostics journal line limit".to_string(),
            false,
        ),
        (
            "answer".to_string(),
            "string".to_string(),
            "Final user-facing answer after a selected tool result is available".to_string(),
            false,
        ),
    ]
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
    let empty_schema = serde_json::json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    });
    let unit_schema = serde_json::json!({
        "type": "object",
        "required": ["unit"],
        "properties": {
            "unit": {
                "type": "string",
                "description": "Systemd unit name to inspect. The app rejects unsafe names."
            }
        },
        "additionalProperties": false
    });
    let lines_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "lines": {
                "type": "integer",
                "description": "Maximum recent journal lines to collect, clamped to 20..300"
            }
        },
        "additionalProperties": false
    });
    let unit_lines_schema = serde_json::json!({
        "type": "object",
        "required": ["unit"],
        "properties": {
            "unit": {
                "type": "string",
                "description": "Systemd unit name to inspect. The app rejects unsafe names."
            },
            "lines": {
                "type": "integer",
                "description": "Maximum recent journal lines to collect, clamped to 20..300"
            }
        },
        "additionalProperties": false
    });

    let specs = [
        (
            "get_kernel_os_version",
            "Run uname -a to read kernel and OS version information.",
            &empty_schema,
        ),
        (
            "get_uptime_load",
            "Run uptime to read uptime and load averages.",
            &empty_schema,
        ),
        (
            "get_disk_usage",
            "Run df for mounted disk usage, excluding tmpfs and devtmpfs.",
            &empty_schema,
        ),
        (
            "get_memory_summary",
            "Run free -h to read memory and swap summary.",
            &empty_schema,
        ),
        (
            "get_swap_devices",
            "Run swapon --show to read configured swap devices.",
            &empty_schema,
        ),
        (
            "get_failed_system_units",
            "Run systemctl --failed to read failed system units.",
            &empty_schema,
        ),
        (
            "get_system_unit_status",
            "Run systemctl status for one safe system unit name.",
            &unit_schema,
        ),
        (
            "get_system_unit_journal",
            "Run journalctl for one safe system unit name.",
            &unit_lines_schema,
        ),
        (
            "get_recent_system_warnings",
            "Run journalctl for recent system warning messages.",
            &lines_schema,
        ),
        (
            "get_recent_kernel_warnings",
            "Run journalctl -k for recent kernel warning messages.",
            &lines_schema,
        ),
        (
            "get_failed_user_units",
            "Run systemctl --user --failed to read failed user units.",
            &empty_schema,
        ),
        (
            "get_user_unit_status",
            "Run systemctl --user status for one safe user unit name.",
            &unit_schema,
        ),
        (
            "get_user_unit_journal",
            "Run user journalctl for one safe user unit name.",
            &unit_lines_schema,
        ),
        (
            "get_recent_user_warnings",
            "Run journalctl --user for recent user-session warning messages.",
            &lines_schema,
        ),
    ];

    specs
        .into_iter()
        .map(|(name, description, schema)| {
            Ok(ToolDefinitionDbus {
                name: name.to_string(),
                description: description.to_string(),
                schema_json: serde_json::to_string(schema)?,
            })
        })
        .collect()
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

fn parse_guided_multi_tool_choice_response(
    content: &str,
    prompt: &str,
) -> anyhow::Result<GuidedMultiToolChoiceResponse> {
    let mut response = serde_json::from_str::<GuidedMultiToolChoiceResponse>(content)?;
    if response.tool_name.trim().is_empty() || response.tool_name == "stub" {
        response.tool_name = infer_multi_tool_name(prompt).to_string();
    }
    if response.tool_name == "count_character_occurrences" {
        if response.word.trim().is_empty() || response.word == "stub" {
            response.word = infer_word_from_prompt(prompt).unwrap_or_default();
        }
        if response.character.trim().is_empty() || response.character == "stub" {
            response.character = infer_character_from_prompt(prompt)
                .unwrap_or('r')
                .to_string();
        }
    }
    Ok(response)
}

fn infer_multi_tool_name(prompt: &str) -> &'static str {
    let lower = prompt.to_lowercase();
    if lower.contains("diagnostic")
        || lower.contains("linux")
        || lower.contains("journal")
        || lower.contains("systemd")
        || lower.contains("failed unit")
    {
        "get_recent_system_warnings"
    } else {
        "count_character_occurrences"
    }
}

fn multi_tool_arguments_from_response(response: &GuidedMultiToolChoiceResponse) -> String {
    if response.tool_name != "count_character_occurrences" {
        return diagnostics_arguments_from_response(&GuidedDiagnosticsLoopResponse {
            action: "call_tool".to_string(),
            tool_name: response.tool_name.clone(),
            scope: response.scope.clone(),
            unit: response.unit.clone(),
            lines: response.lines,
            tool_input: String::new(),
            answer: response.answer.clone(),
        });
    }

    serde_json::json!({
        "word": response.word,
        "character": response.character,
    })
    .to_string()
}

fn execute_multi_tool_call(
    prompt: &str,
    tool_name: &str,
    arguments_json: &str,
) -> anyhow::Result<serde_json::Value> {
    match tool_name {
        "count_character_occurrences" => execute_count_tool(prompt, arguments_json),
        "collect_linux_pc_diagnostics" => execute_linux_pc_diagnostics_tool(arguments_json),
        _ => execute_linux_diagnostics_tool_call(tool_name, arguments_json),
    }
}

fn format_multi_tool_result_for_trace(tool_name: &str, result: &serde_json::Value) -> String {
    if tool_name != "count_character_occurrences" {
        format_diagnostics_tool_result_for_trace(result)
    } else {
        result.to_string()
    }
}

fn format_tool_result_answer(result: &serde_json::Value) -> String {
    let word = result["word"].as_str().unwrap_or("the input");
    let character = result["character"].as_str().unwrap_or("?");
    let count = result["count"].as_u64().unwrap_or_default();
    format!("The character '{character}' occurs {count} times in {word}.")
}

fn format_multi_tool_result_answer(tool_name: &str, result: &serde_json::Value) -> String {
    if tool_name != "count_character_occurrences" {
        format_linux_pc_diagnostics_answer(result)
    } else {
        format_tool_result_answer(result)
    }
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
        guided_linux_pc_diagnostics_loop_fields, guided_multi_tool_choice_fields,
        guided_tool_loop_fields, initial_final_answer, is_safe_systemd_unit,
        linux_pc_diagnostics_tool_definitions, multi_tool_arguments_from_response,
        parse_guided_diagnostics_loop_response, parse_guided_multi_tool_choice_response,
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
    fn diagnostics_tools_are_split_by_command_family() {
        let names = linux_pc_diagnostics_tool_definitions()
            .expect("diagnostics tools should serialize")
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"get_kernel_os_version".to_string()));
        assert!(names.contains(&"get_disk_usage".to_string()));
        assert!(names.contains(&"get_recent_kernel_warnings".to_string()));
        assert!(!names.contains(&"collect_linux_pc_diagnostics".to_string()));
    }

    #[test]
    fn multi_tool_choice_schema_requires_only_tool_name() {
        let required_fields = guided_multi_tool_choice_fields()
            .into_iter()
            .filter_map(|(name, _, _, required)| required.then_some(name))
            .collect::<Vec<_>>();

        assert_eq!(required_fields, vec!["tool_name"]);
    }

    #[test]
    fn multi_tool_choice_fallback_selects_count_tool_arguments() {
        let response = parse_guided_multi_tool_choice_response(
            r#"{"tool_name":"stub","word":"stub","character":"stub","answer":""}"#,
            "Use count_character_occurrences to count the letter s in Mississippi",
        )
        .expect("stub multi-tool response should be repaired");
        let arguments = serde_json::from_str::<serde_json::Value>(
            &multi_tool_arguments_from_response(&response),
        )
        .expect("arguments should be JSON");

        assert_eq!(response.tool_name, "count_character_occurrences");
        assert_eq!(arguments["word"], "Mississippi");
        assert_eq!(arguments["character"], "s");
    }

    #[test]
    fn multi_tool_choice_fallback_selects_diagnostics_tool() {
        let response = parse_guided_multi_tool_choice_response(
            r#"{"tool_name":"stub","answer":""}"#,
            "Analyze this Linux PC for failed systemd units",
        )
        .expect("stub multi-tool response should be repaired");

        assert_eq!(response.tool_name, "get_recent_system_warnings");
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
