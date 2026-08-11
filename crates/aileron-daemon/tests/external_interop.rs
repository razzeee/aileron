use aileron_daemon::assignments::Assignments;
use aileron_daemon::config::Config;
use aileron_daemon::container::ContainerPool;
use aileron_daemon::hardware::Variant;
use aileron_daemon::manifests::RuntimeManifestStore;
use aileron_daemon::permissions::PermissionStore;
use aileron_daemon::profiles::ProfileStore;
use aileron_daemon::service::AileronService;
use aileron_daemon::state::{Inner, SharedState};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::Mutex;

const INTERFACES: [(&str, &str, &str); 4] = [
    (
        "aileron.Inference",
        "type ModelAvailability",
        "method GetUseCaseAvailability",
    ),
    ("aileron.Models", "type ProfileInfo", "method List"),
    (
        "aileron.Permissions",
        "type AppPermission",
        "method ListAppPermissions",
    ),
    ("aileron.Sessions", "type SessionInfo", "method KillSession"),
];

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
async fn external_clients_interoperate_with_production_service() {
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
    let socket = directory.path().join("aileron.socket");
    let listener = zlink::tokio::unix::bind(&socket).unwrap();
    let server = zlink::Server::new(listener, AileronService::new(empty_state(directory.path())));
    let client_test = async {
        for client in clients {
            let socket = socket.clone();
            tokio::task::spawn_blocking(move || exercise_client(client, &socket))
                .await
                .unwrap();
        }
    };
    tokio::select! {
        result = server.run() => panic!("production service exited early: {result:?}"),
        () = client_test => {}
    }
}

fn exercise_client(client: Client, socket: &Path) {
    let address = format!("unix:{}", socket.display());
    let info = match client {
        Client::Varlinkctl => run_ok("varlinkctl", ["info", &address]),
        Client::RustVarlink => run_ok("varlink", ["info", &address]),
    };
    for expected in [
        "aileron",
        "Aileron local AI daemon",
        env!("CARGO_PKG_VERSION"),
        "https://github.com/aileron-project/aileron",
    ] {
        assert_contains(&info, expected, client, "service GetInfo metadata");
    }
    for (interface, _, _) in INTERFACES {
        assert_contains(&info, interface, client, "service GetInfo interfaces");
    }

    for (interface, custom_type, method) in INTERFACES {
        let introspection = match client {
            Client::Varlinkctl => run_ok("varlinkctl", ["introspect", &address, interface]),
            Client::RustVarlink => {
                let target = format!("{address}/{interface}");
                run_ok("varlink", ["help", &target])
            }
        };
        assert_contains(&introspection, interface, client, "interface name");
        assert_contains(&introspection, custom_type, client, "interface type");
        assert_contains(&introspection, method, client, "interface method");
    }

    let availability = call(
        client,
        &address,
        "aileron.Inference.GetUseCaseAvailability",
        r#"{"app_id":"external","use_case":"not.supported"}"#,
    );
    assert!(
        availability.status.success(),
        "{} ordinary call failed: {}",
        client.label(),
        combined_output(&availability)
    );
    let availability: serde_json::Value = serde_json::from_slice(&availability.stdout)
        .unwrap_or_else(|error| {
            panic!(
                "{} returned invalid JSON for ordinary call: {error}; output: {}",
                client.label(),
                combined_output(&availability)
            )
        });
    assert_eq!(availability["availability"]["is_available"], false);
    assert_eq!(availability["availability"]["code"], "unsupported_use_case");

    let missing = call(
        client,
        &address,
        "aileron.Sessions.KillSession",
        r#"{"session_id":"missing"}"#,
    );
    assert!(
        !missing.status.success(),
        "{} accepted KillSession for a missing session",
        client.label()
    );
    let missing = combined_output(&missing);
    assert_contains(
        &missing,
        "aileron.Sessions.SessionNotFound",
        client,
        "declared error",
    );
    assert_contains(&missing, "missing", client, "declared error fields");
}

fn empty_state(root: &Path) -> SharedState {
    let config = Config {
        allow_all: true,
        auto_grant: false,
        idle_timeout_secs: 300,
        container_memory: "1g".into(),
        oci_store: Some(root.join("oci")),
    };
    let mut containers = ContainerPool::new();
    containers.oci_store = root.join("oci");
    SharedState(
        Arc::new(Mutex::new(Inner {
            config,
            permissions: PermissionStore::default(),
            assignments: Assignments::default(),
            profiles: ProfileStore::default(),
            profile_epochs: HashMap::new(),
            runtimes: RuntimeManifestStore::default(),
            sessions: HashMap::new(),
            installing_profiles: HashMap::new(),
            runtime_downloads: HashMap::new(),
            runtime_download_owners: HashMap::new(),
            runtime_update_checks: HashMap::new(),
            recent_installs: VecDeque::new(),
            recent_runtime_downloads: VecDeque::new(),
            variant: Variant::Cpu,
        })),
        Arc::new(StdMutex::new(HashMap::new())),
        Arc::new(Mutex::new(containers)),
        Arc::new(StdMutex::new(HashSet::new())),
        Arc::new(StdMutex::new(HashMap::new())),
        Arc::new(StdMutex::new(HashMap::new())),
        Arc::new(StdMutex::new(HashMap::new())),
    )
}

fn call(client: Client, address: &str, method: &str, parameters: &str) -> Output {
    match client {
        Client::Varlinkctl => Command::new("varlinkctl")
            .args(["--json=short", "call", address, method, parameters])
            .output()
            .unwrap(),
        Client::RustVarlink => Command::new("varlink")
            .args(["call", &format!("{address}/{method}"), parameters])
            .output()
            .unwrap(),
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
