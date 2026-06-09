use clap::Parser;

/// Aileron local AI daemon.
#[derive(Parser, Debug, Clone)]
#[command(version, about)]
pub struct Config {
    /// Allow all inference requests without checking permissions.
    /// Intended for development and testing only.
    #[arg(long, env = "AILERON_ALLOW_ALL", default_value_t = false)]
    pub allow_all: bool,

    /// Automatically grant permission to an app on its first request
    /// instead of denying it. The grant is persisted to permissions.json.
    #[arg(long, env = "AILERON_AUTO_GRANT", default_value_t = false)]
    pub auto_grant: bool,

    /// Container idle timeout in seconds before it is terminated.
    #[arg(long, env = "AILERON_IDLE_TIMEOUT_SECS", default_value_t = 300)]
    pub idle_timeout_secs: u64,
}
