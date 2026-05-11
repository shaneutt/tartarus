# Development Conventions

## Coding Style

### General Principles

- Brevity is a component of quality. Keep code lean and
  complete; no bloat.
- Small, composable, single-purpose functions are the
  default unit of organization. Split code into small
  files with focused responsibilities.
- Minimize side effects. Prefer pure transformations when
  feasible: data in, data out. Resist mutable state when
  feasible and outside the critical paths.
- Keep functions short enough to reason about in isolation.

### Important Tools

- **Clippy**: Enforce idiomatic Rust and catch common mistakes.
- **rustfmt**: Ensure consistent code formatting.
- **cargo-audit**: Check for vulnerable dependencies.
- **cargo-deny**: Enforce supply chain safety policies.
- **rustdoc**: Generate the API documentation.

### Comments vs Tracing

Comments answer **"why?"**, never **"what?"**.

**"What?" belongs in `tracing`**, not comments. If a
comment describes what the code is doing at runtime
("connect to libvirt", "boot the domain", "skip the
snapshot"), replace it with a `tracing::debug!`,
`tracing::trace!`, or `tracing::info!` call. Runtime
narration (what the code did, what it decided, what it
skipped) is structured logging, not commentary.

**"Why?" belongs in comments**, but only when
non-obvious. A hidden constraint, a subtle invariant, a
workaround for a specific bug, or behavior that would
surprise a reader: these justify a comment. If removing
the comment would not confuse a future reader, do not
write it.

**"What?" at the code level needs neither.** Well-named
identifiers already explain what the code does. Do not
write comments that restate what names already convey.

### Testing

**New capabilities require all of the following:**

1. Unit tests covering the implementation.
2. Integration tests proving end-to-end behavior against
   a real `libvirtd` (the test environment is responsible
   for providing one; tests must not silently no-op when
   it is missing).
3. A worked example under `examples/` where the capability
   is exercised by something a user could plausibly run.

This is not optional. A feature without tests is not
complete.

Prefer more doctests when in doubt. Duplicative coverage
between doctests and unit/integration tests is fine.

Prefer assertion messages over inline comments. Put the
explanation in the assertion's message argument so it
prints on failure:

```rust
// Bad:
// domain should be running after start()
assert_eq!(domain.state()?, DomainState::Running);

// Good:
assert_eq!(
    domain.state()?,
    DomainState::Running,
    "domain should be running after start()",
);
```

### API & Spec Conformance

When implementing virtualization-level behavior (libvirt
API usage, QEMU/QMP semantics, KVM ioctls, virtio
devices, vsock, etc.), identify the governing
specification or upstream API contract and verify
conformance against it.

- Cite the specific document, section, or upstream API
  function in test names or doc comments for conformance
  tests.
- When in doubt about an edge case, the upstream
  specification or libvirt source is the authority, not
  other tools' behavior.
- Add dedicated conformance tests when implementing
  spec-defined behavior.

### Rules, Practices & Lints

Security is enforced at the lint level. Lints are
declared in the workspace `Cargo.toml` once the workspace
is scaffolded.

- `#![deny(unsafe_code)]` in all crate roots. No
  exceptions inside Tartarus crates; unsafe belongs
  upstream (e.g. inside `virt-sys`).

  **Exception**: `tartarus/src/host/signals.rs` is
  annotated `#![allow(unsafe_code)]` to install POSIX
  signal handlers via raw `extern "C"` declarations.
  Rust's standard library does not expose
  async-signal-safe handler installation without either
  `unsafe` or a third-party crate; the project's
  dependency rule rules out the latter, and the
  self-pipe trick that wakes the console-attach detach
  path needs the former. The exception is narrow — it
  is the only place in the workspace where
  `unsafe_code` is allowed, and the module's doc
  comment carries the rationale.
- Clippy runs with `-D warnings` (zero tolerance).
- All items (public and private) require `///` doc
  comments; enforced by the `missing_docs` lint.
- Errors via `thiserror`. No string errors, no `Box<dyn Error>`
  in public APIs.
- Logging via `tracing`. Never `println!` / `eprintln!`
  in library or runtime code.
- Use workspace dependencies (`[workspace.dependencies]`)
  to keep versions consistent across crates.
- Keep dependencies light. **No dependency may be added,
  upgraded, or replaced without explicit maintainer
  permission** — see [`../CLAUDE.md`](../CLAUDE.md). The
  praxis-style rule "only well-established crates" is the
  floor, not the ceiling.
- `cargo audit` and `cargo deny check` enforce supply
  chain safety once CI is in place.

#### Additional Coding Conventions

- **Separator comments**: use full-width three-line
  separators to delineate logical sections. Section names
  must be **semantic** (describe the contents), not
  visibility-based. For example: `// HostUser`,
  `// Validation`, `// Utility Functions`, `// Tests`
  rather than `// Public API` or `// Private Utilities`.
  ```rust
  // -----------------------------------------------------------
  // Section Name
  // -----------------------------------------------------------
  ```
- **No re-export-only files.** If a file exists solely to
  `pub use` items from another crate or module, inline
  the import at the call site instead.
- **Constants** must be at the top of the file (after
  imports), never inside functions or impl blocks.
- **File ordering**:
  1. Constants
  2. Primary types, impls, and functions
  3. Supporting types and impls
  4. Utility functions
  5. `#[cfg(test)] mod tests` block (always last)
- **Field and method ordering**: Alphabetical, with
  `name` pinned first on structs and `new()`/`name()`
  pinned first in impl blocks. **Exception**: `clap`-derive
  argument structs follow CLI-affordance order (positional /
  required first, flags grouped, subcommand last). Idiomatic
  command-line layout wins over alphabetisation when the
  struct is a clap surface.
- **Inside `#[cfg(test)] mod tests`**:
  1. Imports
  2. All test functions (`#[test]` / `#[tokio::test]`)
  3. Test utilities at the end (with `// Test Utilities`
     separator)
- No inline comments in test bodies. No doc comments on
  test functions. Use full-width separators only.
- Place a blank line between attribute blocks.
- Separate distinct logical actions with blank lines.
  Function calls, variable bindings that begin a new
  step, and expression blocks that perform a discrete
  operation should have some newline space.
- Prefer `to_owned()` over `to_string()` for `&str` to
  `String`.
- Use inline format args: `format!("{var}")`, not
  `format!("{}", var)`.
- Use let-chains, `is_some_and()`, `strip_prefix()`, and
  other modern idioms when they make the code clearer.
- Reference-style rustdoc links, not inline.
- Do not document memory efficiency in rustdoc (e.g.
  "avoids allocation", "zero-copy", "cheap clone").
  Correct memory use is expected; it does not need
  narration.
- Prefer pre-computed numeric literals over expressions
  like `1024 * 10`. Always add a trailing comment with
  the human-readable size or meaning (e.g.
  `const MAX_DOMAIN_XML: usize = 1_048_576; // 1 MiB`).

## Code Responsibility

This project does not distinguish between code written by
hand, generated by a tool (e.g. lint), or produced by any
other means. **Every contributor is responsible for the
code they submit**, and *all* code MUST be human reviewed
before submission, or merging.

Signed-off commits (`Signed-off-by:`) are required and
represent your assertion that you have reviewed and fully
understand the changes you are submitting.

PRs from a bot or tool (with the exception of GitHub-specific
ones like `dependabot`) will not be accepted.

Before submitting or merging PRs, ensure that you have:

- Read every line of the diff. If you cannot explain why
  something exists, do not submit it.
- Verified that the change does what you intended and
  nothing more.
- Run the test suite *locally* first. CI is not a
  substitute for local verification.

> **Note**: `Draft` pull requests are not exempt from
> these guidelines. They are still expected to be
> reviewed before submission.
