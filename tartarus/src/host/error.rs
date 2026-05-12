//! Errors raised by the [`crate::host`] subsystem.

// -----------------------------------------------------------------------------
// HostError
// -----------------------------------------------------------------------------

/// Failure modes specific to the libvirt connect / domain / agent surface.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The domain XML never declared a `qemu-guest-agent` channel.
    /// Configuration error, not transient.
    #[error(
        "qemu-guest-agent channel is not declared in this domain. hint: this is a session-creation bug — re-define \
         the domain with the guest agent virtio-serial channel."
    )]
    AgentChannelMissing,

    /// In-guest command exited non-zero or did not exit within the
    /// poll window (`code = -1`).
    #[error("in-guest command failed: {detail} (exit {code})")]
    AgentExecFailed {
        /// Exit code, or `-1` when the process did not exit.
        code: i64,

        /// Static label describing the failure.
        detail: &'static str,
    },

    /// Agent channel exists but the guest agent is not responding.
    /// Transient; retry after boot completes.
    #[error(
        "qemu-guest-agent did not respond: {detail}. hint: try `tartarus stop <session>` followed by `tartarus run` \
         (or `tartarus resume`) to restart the session and bring the agent back online."
    )]
    AgentNotResponding {
        /// Libvirt error message.
        detail: String,
    },

    /// Malformed or unexpected reply from `qemu-guest-agent`.
    #[error("qemu-guest-agent protocol error: {detail}")]
    AgentProtocol {
        /// Description of the malformed reply.
        detail: String,
    },

    /// Could not reach `libvirtd` at the configured URI.
    #[error(
        "could not connect to libvirt at {uri}: {source}. hint: is `libvirtd` running on the user session bus? try \
         `systemctl --user status libvirtd`."
    )]
    Connect {
        /// Underlying libvirt error.
        source: virt::error::Error,

        /// URI that was attempted.
        uri: String,
    },

    /// `stty` returned non-zero during terminal mode manipulation.
    #[error("`stty {operation}` failed: {detail}")]
    ConsoleSttyFailed {
        /// Which `stty` step failed (`capture`, `raw`, `restore`).
        operation: &'static str,

        /// Trimmed stderr from `stty`.
        detail: String,
    },

    /// A libvirt domain operation failed.
    #[error("libvirt domain operation `{operation}` failed: {source}")]
    DomainOperation {
        /// Label identifying the failed operation.
        operation: &'static str,

        /// Underlying libvirt error.
        source: virt::error::Error,
    },

    /// Domain did not reach shut-off within the timeout.
    #[error("domain `{name}` did not reach shut-off within {seconds} seconds")]
    ShutdownTimeout {
        /// Domain name.
        name: String,

        /// Seconds waited.
        seconds: u64,
    },
}
