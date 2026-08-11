use aileron_varlink::inference::{
    CreateSession_Reply, Error as InferenceError, GetUseCaseAvailability_Reply, ModelAvailability,
    ResponseOptions, StreamResponse_Reply,
};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

const INTERFACE: &str = "aileron.Inference";

struct InteropFixture;

#[zlink::service(
    interface = "aileron.Inference",
    types = [ModelAvailability, ResponseOptions],
    vendor = "Aileron",
    product = "zlink external interoperability fixture",
    version = "1",
    url = "https://github.com/aileron-project/aileron"
)]
impl InteropFixture {
    async fn get_use_case_availability(
        &self,
        app_id: String,
        use_case: String,
    ) -> GetUseCaseAvailability_Reply {
        GetUseCaseAvailability_Reply {
            availability: ModelAvailability {
                is_available: true,
                code: "available".into(),
                reason: format!("{app_id} may use {use_case}"),
            },
        }
    }

    async fn create_session(
        &self,
        app_id: String,
        use_case: String,
        instructions: String,
    ) -> Result<CreateSession_Reply, InferenceError> {
        let _ = instructions;
        Err(InferenceError::PermissionDenied { app_id, use_case })
    }

    #[zlink(more)]
    async fn stream_response(
        &self,
        more: bool,
        session_id: String,
        input_json: String,
        media_paths: Vec<String>,
        options: ResponseOptions,
    ) -> impl zlink::futures_util::Stream<Item = zlink::Reply<StreamResponse_Reply>> + Unpin {
        let _ = (session_id, media_paths, options);
        let last = if more { 3 } else { 1 };
        zlink::futures_util::stream::iter((1..=last).map(move |value| {
            zlink::Reply::new(Some(StreamResponse_Reply {
                token: format!("{input_json}-{value}"),
            }))
            .set_continues(Some(value < last))
        }))
    }
}

#[derive(Clone, Copy)]
enum Client {
    Varlinkctl,
    RustVarlink,
}

impl Client {
    fn binary(self) -> &'static str {
        match self {
            Self::Varlinkctl => "varlinkctl",
            Self::RustVarlink => "varlink",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Varlinkctl => "systemd varlinkctl",
            Self::RustVarlink => "Rust varlink CLI",
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn external_clients_interoperate_with_zlink() {
    let required = std::env::var_os("AILERON_REQUIRE_EXTERNAL_VARLINK_CLIENTS").is_some();
    let clients = [Client::Varlinkctl, Client::RustVarlink];
    let missing = clients
        .iter()
        .copied()
        .filter(|client| !command_exists(client.binary()))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let names = missing
            .iter()
            .map(|client| client.label())
            .collect::<Vec<_>>()
            .join(", ");
        if required {
            panic!("required external Varlink clients are absent: {names}");
        }
        eprintln!("skipping external interoperability test; missing: {names}");
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("interop.socket");
    let listener = zlink::tokio::unix::bind(&socket).unwrap();
    let server = zlink::Server::new(listener, InteropFixture);
    let client_test = async {
        for client in clients {
            let socket = socket.clone();
            tokio::task::spawn_blocking(move || exercise_client(client, &socket))
                .await
                .unwrap();
        }
    };
    tokio::select! {
        result = server.run() => panic!("fixture server exited early: {result:?}"),
        () = client_test => {}
    }
}

fn exercise_client(client: Client, socket: &Path) {
    let address = format!("unix:{}", socket.display());

    let info = match client {
        Client::Varlinkctl => run_ok("varlinkctl", ["info", &address]),
        Client::RustVarlink => run_ok("varlink", ["info", &address]),
    };
    assert_contains(&info, "Aileron", client, "service GetInfo");
    assert_contains(&info, INTERFACE, client, "service GetInfo interfaces");

    let introspection = match client {
        Client::Varlinkctl => run_ok("varlinkctl", ["introspect", &address, INTERFACE]),
        Client::RustVarlink => {
            let target = format!("{address}/{INTERFACE}");
            run_ok("varlink", ["help", &target])
        }
    };
    assert_contains(
        &introspection,
        "method GetUseCaseAvailability",
        client,
        "interface introspection",
    );
    assert_contains(
        &introspection,
        "error PermissionDenied",
        client,
        "interface introspection",
    );

    let method = format!("{INTERFACE}.GetUseCaseAvailability");
    let echo = call(
        client,
        &address,
        &method,
        r#"{"app_id":"external","use_case":"language.generate"}"#,
        false,
    );
    let echo = parse_json_values(&echo.stdout, client, "ordinary call");
    assert_eq!(echo.len(), 1, "{} ordinary reply count", client.label());
    assert_eq!(echo[0]["availability"]["code"], "available");
    assert_eq!(echo[0]["availability"]["is_available"], true);

    let method = format!("{INTERFACE}.CreateSession");
    let rejected = call(
        client,
        &address,
        &method,
        r#"{"app_id":"external","use_case":"language.generate","instructions":"test"}"#,
        false,
    );
    assert!(
        !rejected.status.success(),
        "{} accepted CreateSession",
        client.label()
    );
    let rejected_output = combined_output(&rejected);
    assert_contains(
        &rejected_output,
        &format!("{INTERFACE}.PermissionDenied"),
        client,
        "typed error",
    );
    assert_contains(&rejected_output, "external", client, "typed error fields");
    let method = format!("{INTERFACE}.StreamResponse");
    let stream = call(
        client,
        &address,
        &method,
        r#"{"session_id":"test","input_json":"token","media_paths":[],"options":{"maximum_response_tokens":3,"temperature":0.0,"source_language_hint":"","target_language_hint":"","execution_mode":"interactive"}}"#,
        true,
    );
    assert!(
        stream.status.success(),
        "{} stream failed: {}",
        client.label(),
        combined_output(&stream)
    );
    let replies = parse_json_values(&stream.stdout, client, "multi-reply stream");
    assert_eq!(replies.len(), 3, "{} stream reply count", client.label());
    assert_eq!(
        replies
            .iter()
            .map(|reply| reply["token"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["token-1", "token-2", "token-3"]
    );
}

fn call(client: Client, address: &str, method: &str, parameters: &str, more: bool) -> Output {
    match client {
        Client::Varlinkctl => {
            let mut command = Command::new("varlinkctl");
            command.arg("--json=short");
            if more {
                command.arg("--more");
            }
            command.args(["call", address, method, parameters]);
            command.output().unwrap()
        }
        Client::RustVarlink => {
            let target = format!("{address}/{method}");
            let mut command = Command::new("varlink");
            command.arg("call");
            if more {
                command.arg("--more");
            }
            command.args([&target, parameters]);
            command.output().unwrap()
        }
    }
}

fn run_ok<I, S>(program: &str, args: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} failed: {}",
        combined_output(&output)
    );
    combined_output(&output)
}

fn parse_json_values(bytes: &[u8], client: Client, operation: &str) -> Vec<serde_json::Value> {
    // varlinkctl uses RFC 7464 JSON text sequences for multiple replies.
    let normalized = bytes
        .iter()
        .map(|byte| if *byte == 0x1e { b' ' } else { *byte })
        .collect::<Vec<_>>();
    serde_json::Deserializer::from_slice(&normalized)
        .into_iter()
        .map(|value| {
            value.unwrap_or_else(|error| {
                panic!(
                    "{} returned invalid JSON for {operation}: {error}; output: {}",
                    client.label(),
                    String::from_utf8_lossy(bytes)
                )
            })
        })
        .collect()
}

fn assert_contains(output: &str, expected: &str, client: Client, operation: &str) {
    assert!(
        output.contains(expected),
        "{} {operation} output did not contain {expected:?}: {output}",
        client.label()
    );
}

fn combined_output(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn command_exists(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}
