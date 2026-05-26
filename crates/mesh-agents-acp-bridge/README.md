# mesh-agents-acp-bridge

ACP harness bridge primitives for mesh-llm A2A agents.

This crate owns ACP command planning and, as implementation continues, ACP event
translation and harness session behavior. It depends on the official
`agent-client-protocol` crate for protocol boundaries.

It should not own A2A HTTP routing, mesh discovery, or CLI parsing.
