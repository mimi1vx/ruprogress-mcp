# ADR 0003: MSRV floor raised to 1.96

## Context

The workspace originally set `rust-version = "1.88"`, the floor required
by `rmcp` 3.x. The local toolchain available for development and verification
is 1.97.1; no 1.88 toolchain is installed, and per the sota-rust skill's
tooling policy, arbitrary historical MSRV toolchains are not installed
without asking.

## Decision

Raise the workspace MSRV floor to **1.96**, deliberately, rather than
maintaining an unverified 1.88 claim. This is still comfortably above rmcp
3.x's 1.88 requirement, and the CI `msrv` job pins 1.96 accordingly (CI
provisions that toolchain fresh; it does not depend on what happens to be
installed locally).

## Consequence

If a future dependency requires exactly a sub-1.96 floor to be documented
(e.g. for downstream consumers on older toolchains), revisit — 1.88 remains
the technical minimum per rmcp, 1.96 is this project's chosen floor.
