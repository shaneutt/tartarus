//! Phase 3 integration tests: real-libvirtd round-trips.
//!
//! Every test in this file is `#[ignore]`'d because the build environment
//! the project targets for unit-test runs (`make test`) does not have a
//! running `libvirtd`. A developer who has set up `qemu:///session` locally
//! runs this suite with:
//!
//! ```console
//! cargo test --test host_phase3 -- --ignored
//! ```

use std::time::Duration;

use tartarus::host::{
    agent::Agent,
    connect::{Connection, DEFAULT_URI},
    domain::{self, DomainSpec},
};

#[test]
#[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
fn connect_open_succeeds_against_session_libvirtd() {
    let connection = Connection::open(DEFAULT_URI).expect("opening qemu:///session should succeed when libvirtd runs");

    assert!(connection.is_alive(), "freshly opened connection should report alive");
    assert_eq!(connection.uri(), DEFAULT_URI, "uri should round-trip the input");
}

#[test]
#[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
fn define_and_undefine_round_trip_for_a_no_disk_domain() {
    let connection = Connection::open(DEFAULT_URI).expect("opening qemu:///session should succeed when libvirtd runs");

    let name = stable_domain_name("define-undefine");
    let spec = DomainSpec::trivial(&name);

    let domain = domain::define(&connection, &spec).expect("define_xml should succeed for a trivial domain");

    drop(domain);

    domain::undefine(&connection, &spec.name).expect("undefine should succeed for a defined-but-inactive domain");
}

#[test]
#[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
fn agent_ping_dispatches_through_skeleton() {
    let connection = Connection::open(DEFAULT_URI).expect("opening qemu:///session should succeed when libvirtd runs");

    let name = stable_domain_name("agent-ping");
    let spec = DomainSpec::trivial(&name);

    let domain = domain::define(&connection, &spec).expect("define_xml should succeed for a trivial domain");

    let agent = Agent::new(domain);

    let _ = agent.ping(Duration::from_secs(5));

    domain::undefine(&connection, &spec.name).expect("undefine should succeed");
}

#[test]
#[ignore = "requires a running qemu:///session libvirtd plus a real disk; run with --ignored after setting up locally"]
fn start_and_shutdown_round_trip_after_phase_five() {
    let connection = Connection::open(DEFAULT_URI).expect("opening qemu:///session should succeed when libvirtd runs");

    let name = stable_domain_name("start-shutdown");
    let spec = DomainSpec::trivial(&name);

    let _domain = domain::define(&connection, &spec).expect("define should succeed");

    domain::start(&connection, &spec.name).expect("start should succeed once a real disk is wired (P5)");
    domain::shutdown(&connection, &spec.name, Duration::from_secs(30))
        .expect("shutdown should succeed once a guest is running");
    domain::undefine(&connection, &spec.name).expect("undefine should succeed after shutdown");
}

// Test Utilities

fn stable_domain_name(prefix: &str) -> String {
    format!("tartarus-test-{prefix}-{pid}", pid = std::process::id())
}
