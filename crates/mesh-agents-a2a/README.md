# mesh-agents-a2a

Shared A2A agent model code for mesh-llm.

This crate owns directory-only agent discovery, runtime configuration parsing,
and public A2A type boundaries. It depends on the official `a2a-lf`,
`a2a-client-lf`, and `a2a-server-lf` crates rather than defining local protocol
types.

It should not own CLI parsing, harness-specific ACP behavior, or mesh routing.
