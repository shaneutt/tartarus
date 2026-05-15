//! [`LibvirtProvider`]: the libvirt-backed [`SessionProvider`] impl.
//!
//! Holds a resolved [`Config`] and delegates the lifecycle entry
//! points to the free functions already in `crate::session`. The
//! binary's CLI dispatch constructs one [`LibvirtProvider`] per
//! invocation and calls into it through the trait.

use tartarus_provider::{
    DestroyOutcome, ListEntry, RenameOutcome, ResumeOutcome, RunOutcome, RunRequest, SessionProvider, StopOutcome,
    config::Config,
};

use crate::{Error, session};

// -----------------------------------------------------------------------------
// LibvirtProvider
// -----------------------------------------------------------------------------

/// libvirt-backed [`SessionProvider`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibvirtProvider {
    /// Resolved configuration the underlying libvirt code reads
    /// network URI, VM defaults, and credential fields from.
    pub config: Config,
}

impl LibvirtProvider {
    /// Wrap a resolved [`Config`] in a fresh provider.
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl SessionProvider for LibvirtProvider {
    type Error = Error;

    fn run(&self, request: &RunRequest) -> std::result::Result<RunOutcome, Self::Error> {
        session::run::run(&self.config, request)
    }

    fn resume(&self, target: &str) -> std::result::Result<ResumeOutcome, Self::Error> {
        session::resume::run(&self.config, target)
    }

    fn stop(&self, target: &str) -> std::result::Result<StopOutcome, Self::Error> {
        session::stop::run(&self.config, target)
    }

    fn destroy(&self, target: &str) -> std::result::Result<DestroyOutcome, Self::Error> {
        session::destroy::run(&self.config, target)
    }

    fn list(&self) -> std::result::Result<Vec<ListEntry>, Self::Error> {
        session::list::collect(&self.config)
    }

    fn rename(&self, uuid: &str, alias: &str) -> std::result::Result<RenameOutcome, Self::Error> {
        session::rename::run(uuid, alias)
    }
}
