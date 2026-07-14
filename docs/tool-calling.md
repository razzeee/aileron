# Tool Calling And Approval

Aileron supports tool-call-shaped guided generation, but tool execution is app-owned.

## Boundary

The portal and daemon enforce access to local model capabilities at the app/use-case level. For example, an app may need permission to create a `language.analyze` session.

Tool calls are different. The daemon and runtime may return `ToolCall` objects from guided generation, but they do not execute those tools. The portal forwards the tool call to the app. The app decides whether to reject it, ask the user, validate arguments, execute app-local code, and submit `ToolResult` objects back to the model.

Each tool definition has `name`, `description`, and `schema_json`. `schema_json` is a JSON Schema object serialized as a string, describing the JSON object the model should put in `ToolCall.arguments_json`. For example, a calendar lookup tool might use `{"type":"object","required":["date"],"properties":{"date":{"type":"string"}}}`. Treat this as guidance for generation and app validation, not as permission: the app must parse `arguments_json`, validate it against its own registered schema, and reject unknown tools or invalid arguments before executing anything.

```text
app -> StreamRespondGuided(tools)
portal -> daemon -> runtime
runtime -> ToolCall(id, name, arguments_json)
portal -> app
app -> approve/validate/execute or reject
app -> StreamSubmitToolResultsGuided(results)
```

## Demo Pattern

The demo app intentionally asks for approval before local tool execution. Its approval dialog shows the tool name, model-supplied JSON arguments, and demo-specific safety context.

This is an example app pattern, not a portal guarantee. Another app using the same API must implement its own confirmation and validation policy before executing tools.

## App Responsibilities

Apps that use tools should:

- Register narrow tool definitions with clear descriptions and schemas.
- Treat model-supplied arguments as untrusted input.
- Ask for user approval before tools that read sensitive local state, write data, run commands, access the network, or affect other apps.
- Show enough context for the user to make a decision, including the tool name, arguments, expected action, and whether the action is read-only or mutating.
- Validate and constrain arguments before execution.
- Reject unknown tools or unsafe argument combinations.
- Send explicit tool results or cancellation information back to the model instead of silently continuing.

## What The Portal Guarantees

The portal guarantees that app access to model use cases goes through the desktop permission path. It does not guarantee per-tool consent, inspect tool schemas for safety, or run tools in a privileged broker.

That split keeps app-specific authority in the app. It also means tool-using apps must make approval and validation part of their own UX and security model.
