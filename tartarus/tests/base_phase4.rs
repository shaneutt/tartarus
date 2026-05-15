//! Phase 4 integration tests: real-libvirtd base lifecycle.
//!
//! Every test in this file is `#[ignore]`'d because the build environment
//! the project targets for unit-test runs (`make test`) does not have a
//! running `libvirtd`, KVM, or live network access to
//! `download.fedoraproject.org`. A developer who has set up
//! `qemu:///session` locally runs this suite with:
//!
//! ```console
//! cargo test --test base_phase4 -- --ignored
//! ```
//!
//! The host-side orchestration (URL construction, GPG verification flow,
//! qemu-img info parsing, atomic `current` symlink update, layering seed
//! authoring) is exercised by unit tests in `disk::base` and
//! `disk::base::layering_seed`; these integration tests close the loop on
//! the network + libvirt + KVM ends.

use tartarus_libvirt::disk::base;

#[test]
#[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
fn base_pull_completes_against_real_libvirtd() {
    let pulled =
        base::pull(base::DEFAULT_FEDORA_RELEASE).expect("base pull should succeed against a real libvirtd + KVM");

    assert_eq!(
        pulled.release,
        base::DEFAULT_FEDORA_RELEASE,
        "pulled base should carry the requested release",
    );
    assert!(
        pulled.path.exists(),
        "pulled base file should land on disk at {}",
        pulled.path.display(),
    );
}

#[test]
#[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
fn base_pull_then_list_reports_pulled_image_as_current() {
    base::pull(base::DEFAULT_FEDORA_RELEASE).expect("base pull should succeed against a real libvirtd + KVM");

    let library = base::list().expect("base list should succeed after pull");

    assert!(
        library.current.is_some(),
        "current symlink should exist after a successful pull"
    );
    assert!(
        !library.bases.is_empty(),
        "list should include at least one versioned base after pull",
    );
}
