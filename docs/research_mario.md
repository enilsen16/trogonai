# OpenAI Agents SDK — Research Study & Comparison with Trogonai

> **Repository studied:** https://github.com/openai/openai-agents-python
> **Version at time of study:** 0.14.x
> **Date:** 2026-04-19

---

## Table of Contents

1. [What It Is](#1-what-it-is)
2. [Core Architecture](#2-core-architecture)
3. [The Agent Execution Loop](#3-the-agent-execution-loop)
4. [Tool System](#4-tool-system)
5. [Multi-Agent Coordination](#5-multi-agent-coordination)
6. [Guardrails](#6-guardrails)
7. [Sessions and Memory](#7-sessions-and-memory)
8. [Streaming](#8-streaming)
9. [Tracing and Observability](#9-tracing-and-observability)
10. [Extension Points](#10-extension-points)
11. [Similarities with Trogonai](#11-similarities-with-trogonai)
12. [What Trogonai Has That the SDK Does Not](#12-what-trogonai-has-that-the-sdk-does-not)
13. [What the SDK Has That Trogonai Does Not](#13-what-the-sdk-has-that-trogonai-does-not)
14. [What Is Worth Adding to Trogonai](#14-what-is-worth-adding-to-trogonai)

---

## 1. What It Is

The OpenAI Agents SDK is an open-source (MIT license) Python framework for building production multi-agent AI applications. It is the official successor to OpenAI's earlier Swarm prototype. At the time of this study it had 23,000+ GitHub stars, 268 contributors, and 84 releases. It is actively used to power OpenAI's own agent products.

### The problem it solves

Calling an LLM once is trivial. Building a production agent is not. Real workflows require:

- **Turn management** — looping between LLM calls and tool executions until a stop condition
- **Tool dispatch** — routing tool calls, executing functions, appending results, re-invoking the LLM
- **Multi-agent coordination** — delegating work to specialist agents with history preserved
- **Safety** — validating inputs and outputs before they reach users or downstream systems
- **Durability** — pausing and resuming long-running runs across failures or human approvals
- **Observability** — structured tracing of every meaningful operation inside a multi-step run
- **Memory** — persisting and retrieving conversation history with pluggable backends

The SDK covers all of these with five primitives: **Agent, Runner, Tools, Handoffs, Guardrails**.

---

## 2. Core Architecture

```
┌─────────────────────────────────┐
│  Runner  (public API)           │
│  run() / run_sync() /           │
│  run_streamed()                 │
└────────────┬────────────────────┘
             │
┌────────────▼────────────────────┐
│  AgentRunner  (internal loop)   │
│  turn management, tool          │
│  dispatch, handoff switching    │
└────────────┬────────────────────┘
             │
┌────────────▼────────────────────┐
│  Agent · Tool · Handoff         │
│  Guardrail · Session            │
└────────────┬────────────────────┘
             │
┌────────────▼────────────────────┐
│  ModelProvider interface        │
│  OpenAI + 100+ via LiteLLM      │
└─────────────────────────────────┘
```

### Agent

`Agent[TContext]` is a generic dataclass — pure configuration, no execution state.

| Field | Description |
|---|---|
| `name` | Identifier used in handoffs and traces |
| `instructions` | Static string or `async (agent, ctx) -> str` |
| `tools` | Any mix of function tools, MCP tools, hosted tools, agents-as-tools |
| `handoffs` | Other agents this agent can delegate to |
| `guardrails` | Input and output validation chains |
| `output_type` | Optional Pydantic model for structured responses |
| `tool_use_behavior` | Controls when the loop terminates after tool calls |
| `model` | Per-agent model override |

### Runner

The only entry point for execution:

```python
result = await Runner.run(agent, input, context=..., session=...)
result = Runner.run_sync(agent, input)
stream = Runner.run_streamed(agent, input)
```

Accepts a `RunConfig` with ~20 override fields per run: model, input/output filters, tracing config, handoff mappers, tool error formatters, session settings.

### RunResult and RunState

- `RunResult` — final output, new messages, token usage, agent chain
- `RunResultStreaming` — same, exposes an async event iterator
- `RunState` — fully serializable mid-loop snapshot; enables pause-and-resume

---

## 3. The Agent Execution Loop

```
Runner.run(agent, input)
│
├─ 1. Retrieve session history
├─ 2. Resolve dynamic instructions
├─ 3. Run input guardrails
│
└─ LOOP (until max_turns or stop condition):
   │
   ├─ 4. Call LLM
   │
   ├─ end_turn:
   │   ├─ Run output guardrails
   │   ├─ Save session history
   │   └─ Return RunResult
   │
   ├─ tool_use:
   │   ├─ Dispatch tools in parallel
   │   ├─ Apply tool_use_behavior rules
   │   ├─ Append results to messages
   │   └─ Loop → step 4
   │
   └─ handoff tool called:
       ├─ Apply input_filter (trim/transform history)
       ├─ Switch active agent
       └─ Restart loop → step 2
```

### Tool use behavior

The `tool_use_behavior` field on `Agent` controls what happens after tool execution:

| Setting | Effect |
|---|---|
| `run_llm_again` (default) | Always re-invoke LLM after tools |
| `stop_on_first_tool` | Return immediately after first tool call |
| `StopAtTools(["name"])` | Return when any of the listed tools is called |
| custom callable | Developer logic based on tool name + result |

This prevents wasting tokens on unnecessary LLM re-invocations when the goal is a single side-effecting action.

### Pause and resume

`RunState` is fully serializable (versioned JSON schema). A run can pause before executing a tool requiring human approval, persist the state, and resume later with deterministic behavior. No external coordination system needed.

---

## 4. Tool System

The SDK recognizes that tools come from four structurally different sources and gives each its own dispatch logic.

### Function tools

Python functions decorated with `@function_tool`. The decorator auto-generates JSON Schema from type hints, validates inputs via Pydantic, handles errors as LLM-readable strings, and supports per-tool timeouts and rich returns (text + images).

```python
@function_tool
async def search_issues(query: str, project: str = "default") -> str:
    """Search for issues in the tracker."""
    ...
```

### Hosted OpenAI tools

`WebSearchTool`, `FileSearchTool`, `CodeInterpreterTool`, `HostedMCPTool` — execute on OpenAI's infrastructure with no local compute required.

### MCP server tools

MCP servers over 4 transports: OpenAI-hosted, HTTP streamable, HTTP SSE (deprecated), stdio. Tools are discovered via `list_tools()`, prefixed to avoid name collisions, and dispatched transparently alongside function tools.

### Agents as tools

`agent.as_tool()` converts any agent into a callable tool. The sub-agent runs to completion and returns its final text as the tool result. The parent agent retains full control. This is the foundation of the Manager orchestration pattern (see §5).

### Computer and shell tools

`ComputerTool` for GUI/browser automation, `ShellTool` for command execution, `ApplyPatchTool` for unified diffs.

---

## 5. Multi-Agent Coordination

### Handoff pattern — sequential delegation

The LLM calls a handoff tool, the runner switches the active agent, and the loop continues under the new agent's system prompt and tool set. Full conversation history is preserved through transitions.

**Input filter**: a function that transforms conversation history before it reaches the receiving agent — trim tool results, remove sensitive blocks, summarize long exchanges. Prevents context window explosion in long handoff chains.

**Structured handoff input**: `input_type` can be a Pydantic model the LLM populates when triggering the handoff, sending typed metadata to the receiving agent alongside the history.

```
User → Triage Agent
            ├── handoff → Support Agent (with filtered history)
            ├── handoff → Billing Agent
            └── handoff → Sales Agent
```

### Manager pattern — parallel orchestration

A central orchestrator calls specialists as tools via `agent.as_tool()`. Specialists do not take over the conversation — they run and return a result string. The manager synthesizes outputs and maintains full control. Supports parallel execution.

```
Manager Agent
    ├── tool → Specialist A  (sub-agent, returns text)
    ├── tool → Specialist B  (runs in parallel)
    └── synthesizes → final response
```

### Code-based orchestration

The SDK explicitly recommends code-directed routing over LLM-directed routing for deterministic decisions. Patterns: sequential chaining via structured `output_type`, conditional branching on Pydantic fields, feedback loops where an evaluator agent reviews and corrects output.

---

## 6. Guardrails

Three-level validation with tripwire semantics. When a tripwire fires, execution halts immediately and a typed exception propagates to the application layer.

| Level | When it runs | Scope |
|---|---|---|
| **Input guardrail** | Before the first LLM call | First agent only |
| **Output guardrail** | After the final LLM output | Last agent only |
| **Tool guardrail** | Before/after each tool invocation | Per-tool |

Input guardrails can run in two modes:
- **Parallel** (default): concurrent with LLM call — faster, may spend tokens before tripwire fires
- **Blocking**: completes before LLM is invoked — prevents token spend on rejected inputs

```python
@input_guardrail
async def check_policy(agent, input, context) -> GuardrailFunctionOutput:
    if violates_policy(input):
        return GuardrailFunctionOutput(tripwire_triggered=True)
    return GuardrailFunctionOutput(tripwire_triggered=False)
```

**Design rationale**: input guardrails are the most cost-effective — they prevent entire expensive multi-turn runs from starting. Output guardrails add a final safety net. Tool guardrails protect side-effecting operations independently of agent boundaries.

---

## 7. Sessions and Memory

### Session backends

Six pluggable backends implementing a common Protocol interface — swapping is a one-line change in `RunConfig`:

| Backend | Use case |
|---|---|
| `SQLiteSession` | Development, single-machine |
| `RedisSession` | Distributed, horizontally scaled |
| `SQLAlchemySession` | PostgreSQL / MySQL production |
| `DaprSession` | Cloud-native (30+ state store providers) |
| `OpenAIConversationsSession` | OpenAI-managed, minimal ops |
| `EncryptedSession` | AES-256 wrapper over any backend |

### History management controls

- `SessionSettings(limit=N)` — retrieve only the N most recent turns; prevents unbounded growth
- `RunConfig.session_input_callback` — custom function to merge stored history with new input; enables compression or summarization before the LLM sees history
- Handoff input filters — transform the conversation at agent boundaries

### Local context

`RunContextWrapper[TContext]` threads a user-defined context object through all tool calls and hooks within a run. It is **never sent to the LLM**. All agents in a run share the same context type, enforced via generics.

---

## 8. Streaming

`Runner.run_streamed()` emits three distinct event categories:

| Event | Description |
|---|---|
| `RawResponsesStreamEvent` | Raw LLM tokens as they arrive |
| `RunItemStreamEvent` | Complete parsed items (messages, tool calls, outputs) |
| `AgentUpdatedStreamEvent` | Active agent transitions (handoffs) |

Separating raw tokens from semantic items lets UI layers stream character-by-character while business logic waits for complete structured items.

Cancellation: `result.cancel()` for immediate stop or `result.cancel(mode="after_turn")` for graceful completion.

### Voice pipeline

Three-stage audio pipeline: STT → Agent → TTS. Any existing agent plugs into a `VoicePipeline` without modification. Two modes: push-to-talk (full audio clip) and `StreamedAudioInput` (continuous with voice activity detection).

---

## 9. Tracing and Observability

Tracing is **on by default** for every run with no configuration required.

### Semantic span types

| Span | What it captures |
|---|---|
| `AgentSpan` | Full agent execution start to finish |
| `GenerationSpan` | Each LLM API call with token counts |
| `ToolCallSpan` | Each tool invocation with name and input |
| `HandoffSpan` | Each agent delegation event |
| `GuardrailSpan` | Each guardrail evaluation |
| `TurnSpan` | Complete turn (user input → assistant output) |

Spans are hierarchical: a `TurnSpan` nests `GenerationSpan`s and `ToolCallSpan`s.

### Processor pipeline

```python
add_trace_processor(MyExporter())       # add alongside defaults
set_trace_processors([MyExporter()])    # replace all defaults
```

Built-in integrations: Weights & Biases, Langfuse, LangSmith, Arize-Phoenix, MLflow, Braintrust, and 18 others.

Privacy: `trace_include_sensitive_data=False` suppresses message content. Tracing errors never crash agent execution.

---

## 10. Extension Points

Every layer is designed for extension:

- **RunConfig** (~20 fields): model override, input/output filters, handoff mappers, error formatters, tracing config, session settings — all per individual run
- **AgentHooks / RunHooks**: `on_llm_start/end`, `on_tool_start/end`, `on_agent_start/end`, `on_handoff` — injectable at both run-level and agent-level
- **ModelProvider**: factory interface for custom LLM backends
- **Custom tools**: extend `BaseTool` for arbitrary dispatch logic
- **Session backends**: implement the `Session` protocol for any storage layer
- **Guardrail composability**: multiple guardrails form a validation chain, all must pass

---

## 11. Similarities with Trogonai

Despite the language difference (Python vs Rust) and architectural difference (single-process vs distributed), both systems converge on the same core design decisions.

### Agent execution loop

Both implement the same fundamental structure: call LLM → if tool_use, dispatch tools and append results → loop until end_turn. Both cap iterations with `max_iterations` / `max_turns`. The loop in `trogon-agent-core/src/agent_loop.rs` mirrors the SDK's `AgentRunner` internal logic closely.

### MCP integration

Both support Model Context Protocol for tool discovery and dispatch. Trogonai's `trogon-mcp` implements the same JSON-RPC `initialize` / `list_tools` / `call_tool` lifecycle. Both auto-prefix MCP tool names to avoid collisions.

### Streaming events

Both use typed event streams during execution. Trogonai's `AgentEvent` enum (`TextDelta`, `ThinkingDelta`, `ToolCallStarted`, `ToolCallFinished`, `UsageSummary`) maps directly to the SDK's streaming event categories. Both use async channels (mpsc in Rust, async generators in Python).

### Permission gating

Both pause execution before a tool and request user approval. Trogonai's `ChannelPermissionChecker` (mpsc channel to the ACP handler) and the SDK's `needs_approval` / `on_approval` per-tool field solve the same problem.

### Prompt caching

Both explicitly support caching of system prompts and tool definitions. Trogonai marks the last tool with `cache_control: {"type": "ephemeral"}` for Anthropic. The SDK uses OpenAI's equivalent mechanism.

### Context compaction

Both handle long conversation histories through compaction. Trogonai's `trogon-compactor` service via NATS request-reply corresponds to the SDK's `OpenAIResponsesCompactionAwareSession`.

### Session persistence as a replaceable abstraction

Trogonai's `SessionStore` trait backed by NATS KV and the SDK's `Session` Protocol backed by multiple backends solve the same problem in different deployment environments.

### Observability infrastructure

Both integrate with OpenTelemetry for distributed tracing and write structured logs. Both export to external observability platforms.

### Trait / Protocol-based design

Both favor structural abstraction over inheritance. Trogonai uses Rust traits (`SessionStore`, `AgentRunner`, `PermissionChecker`). The SDK uses Python Protocols. Both allow third-party implementations without coupling to internal base classes.

---

## 12. What Trogonai Has That the SDK Does Not

Trogonai is architecturally more sophisticated at the infrastructure layer. These are genuine advantages the SDK cannot replicate without a fundamental redesign.

### Distributed by design

The SDK is single-process. All agents, tools, and sessions live in one Python interpreter. Trogonai is built around NATS JetStream: agents run as independent processes, communicate via message passing, and scale horizontally without code changes. This enables Trogonai to run agents across machines, survive process crashes, and handle high concurrency without Python's GIL.

### Agent Communication Protocol (ACP)

Trogonai implements a formal, versioned agent communication protocol with a full session lifecycle: new, load, resume, fork, close — plus mode switching, model switching, and config updates — all over a NATS transport. The SDK has no inter-agent protocol.

### Session forking

Trogonai supports forking a session into a new session ID, creating a branched conversation copy. The SDK has no equivalent.

### Multi-transport

The same Trogonai agent is reachable via stdio (for CLI/IDE tools), WebSocket (for browser clients), and NATS (for service-to-service communication) using the same protocol. The SDK is coupled to the OpenAI HTTP API.

### Extended thinking

Trogonai has native support for Anthropic's `thinking_budget` parameter and streams `ThinkingDelta` events. The SDK has no equivalent for Claude's extended thinking.

### Event sourcing and crash recovery

NATS JetStream provides persistent, replayable event streams. Session state is immutable and append-only. The platform can recover from process crashes by replaying from JetStream. The SDK has no crash recovery.

### Event ingestion from external systems

`trogon-gateway` provides webhook ingress from GitHub, Slack, Discord, Incident.io, Linear, Sentry, and others, publishing events to JetStream for agent consumption. The SDK has no event ingestion layer.

### Multi-tenancy

NATS KV namespacing and ACP prefix isolation allow multiple tenants to share infrastructure without data leakage. The SDK has no multi-tenancy concept.

---

## 13. What the SDK Has That Trogonai Does Not

These are the meaningful gaps in Trogonai's agentic orchestration layer relative to the SDK.

### Guardrails as a first-class primitive

The SDK has input, output, and tool-level guardrails with tripwire semantics. Trogonai has no validation layer between event ingestion and the LLM, between LLM output and the NATS reply, or around individual tool calls.

### Multi-agent handoffs

The SDK has `Handoff` as a distinct primitive the LLM can invoke to transfer control to another agent, with conversation history preserved and optionally transformed via `input_filter`. Trogonai can route between agents via NATS, but there is no abstraction for the LLM itself to initiate a mid-run delegation with history forwarding.

### Agent-as-tool (Manager pattern)

`agent.as_tool()` lets the parent agent invoke a sub-agent as a tool call. The sub-agent runs to completion and its output becomes the tool result string. The parent retains control. Trogonai has no equivalent.

### Tool use behavior control

The SDK's `tool_use_behavior` field controls when the loop terminates: always continue, stop on first tool, stop on specific named tools, or custom callable. Trogonai's loop always re-invokes the LLM after tool results until `end_turn` or `max_iterations`.

### Structured output typing

An agent can declare `output_type: Type[T]` and the runner validates the final LLM response against that schema. Downstream code branches on typed fields without another LLM call. Trogonai always returns unstructured strings.

### Lifecycle hooks

`RunHooks` and `AgentHooks` provide injectable callbacks at every meaningful execution point: `on_llm_start/end`, `on_tool_start/end`, `on_agent_start/end`, `on_handoff`. Trogonai emits `AgentEvent` for streaming output but has no injectable hooks for external consumers of lifecycle events.

### Dynamic instructions within a run

The SDK formalizes dynamic instructions as a first-class `Agent` field: an async callable receiving the live run context at each turn. Trogonai's `system_prompt` is static per session — infrastructure-level dynamism (updating instructions in NATS KV between sessions) exists, but within a single run the prompt is fixed.

### Semantic trace spans

The SDK emits domain-specific named spans: `AgentSpan`, `GenerationSpan`, `ToolCallSpan`, `HandoffSpan`, `GuardrailSpan`. Trogonai's observability is infrastructure-level (generic `#[instrument]` spans) but lacks spans that capture agentic operations as structured, queryable fields.

### Session history windowing

`SessionSettings(limit=N)` caps history retrieval to N most recent turns. `session_input_callback` enables custom merge and summarize logic at history load time. Trogonai has no turn-level windowing — history grows until hitting the serialization size limit, which triggers a hard error.

### Multiple session backends

The SDK ships six session backends (SQLite, Redis, SQLAlchemy, Dapr, OpenAI-managed, Encrypted). Trogonai has NATS KV as its only backend.

---

## 14. What Is Worth Adding to Trogonai

Ordered by impact relative to implementation effort.

---

### Priority 1 — High impact, clean fit with existing architecture

#### 1.1 Guardrails

Add a validation trait to `trogon-agent-core` and wire it into the agent loop:

```rust
#[async_trait]
pub trait InputGuardrail: Send + Sync {
    async fn check(&self, messages: &[Message], ctx: &RunContext) -> GuardrailResult;
}

#[async_trait]
pub trait OutputGuardrail: Send + Sync {
    async fn check(&self, output: &str, ctx: &RunContext) -> GuardrailResult;
}

pub enum GuardrailResult {
    Pass,
    Block { reason: String },
}
```

Input guardrails run before the first LLM call; output guardrails run before returning from the loop. Tool guardrails wrap individual dispatch calls in `dispatch_tool()`. As Trogonai adds more tenants and event sources with unpredictable prompt content, blocking bad inputs before tokens are spent is critical for cost control and safety.

---

#### 1.2 Tool use behavior control

Add `LoopStopPolicy` to `AgentLoop` and apply it after each tool dispatch before deciding whether to re-invoke the LLM:

```rust
pub enum LoopStopPolicy {
    AlwaysContinue,
    StopOnFirstTool,
    StopAtTools(Vec<String>),
    Custom(Box<dyn Fn(&str, &str) -> bool + Send + Sync>),
}
```

Automations that exist to perform a single action currently spend tokens on a final LLM turn that adds nothing. This eliminates that waste with a one-field configuration change at the automation definition level.

---

#### 1.3 Lifecycle hooks

Add an injectable hooks trait to `AgentLoop`. Default implementations are no-ops:

```rust
#[async_trait]
pub trait AgentHooks: Send + Sync {
    async fn on_llm_start(&self, _messages: &[Message]) {}
    async fn on_llm_end(&self, _response: &AnthropicResponse) {}
    async fn on_tool_start(&self, _name: &str, _input: &Value) {}
    async fn on_tool_end(&self, _name: &str, _output: &str) {}
}
```

The production runner injects a hook that emits semantic trace spans. Test runners inject assertion hooks for verifying tool calls without mocking the LLM.

---

#### 1.4 Semantic trace spans

Replace generic `#[instrument]` blocks with structured domain-specific log events at key points in `agent_loop.rs`:

```rust
tracing::info!(
    target: "trogon.llm_call",
    model = %model,
    input_tokens = usage.input_tokens,
    output_tokens = usage.output_tokens,
    cache_read_tokens = usage.cache_read_input_tokens,
);

tracing::info!(
    target: "trogon.tool_call",
    tool.name = %name,
    tool.input = %serde_json::to_string(&input).unwrap_or_default(),
    tool.source = %if is_mcp { "mcp" } else { "builtin" },
);
```

Makes production traces actionable: cost per tool, latency per LLM call, cache hit rates per automation.

---

### Priority 2 — High value, requires more design work

#### 2.1 Multi-agent handoffs with input filter

Add a `HandoffTool` variant to the tool dispatch path. When the LLM calls it, the loop applies an `input_filter` to the current message history, publishes a new ACP session to the target agent's NATS namespace with the filtered history as initial context, and either returns or transfers execution depending on the handoff type.

The most valuable sub-feature to implement first is the **input filter alone** — even without handoff routing, a function that trims or summarizes conversation history before the next turn addresses context window growth in long-running sessions.

---

#### 2.2 Agent-as-tool

Expose a sub-agent invocation as a `ToolDef`. When dispatched, the runner publishes a prompt to the target agent via ACP/NATS, subscribes to its session notifications, waits for completion with a configurable timeout, and returns the final output as the tool result string. This enables the Manager pattern in a distributed context: one orchestrator agent dispatching multiple specialist agents, then synthesizing their outputs.

---

#### 2.3 Structured output typing

Add `output_schema: Option<serde_json::Value>` to `AgentLoop`. When set, the final LLM call uses Anthropic's tool-based structured output pattern to produce validated JSON. The loop returns a `serde_json::Value` and validates it against the schema before returning. Unlocks code-based routing downstream without a second LLM call.

---

#### 2.4 Session history windowing

Add `max_history_turns: Option<usize>` to `SessionState`. Apply a sliding window when loading messages: keep the system prompt plus the last N turn pairs. When turns are dropped, optionally invoke the compactor to summarize them and prepend the summary as a synthetic context block. This converts the current hard error on size limit into graceful degradation.

---

### Priority 3 — Lower urgency, longer term

#### 3.1 RunState / human-in-the-loop pause

Add a `Suspended` status to the session status enum and a dedicated NATS request-reply subject for approval handshakes. The agent pauses before a high-risk tool, writes a `Suspended` state, and resumes only when an external approval signal arrives. Trogonai's NATS KV already handles the persistence — this is primarily an API and status enum extension.

#### 3.2 Dynamic instructions within a run

Add a `SystemPromptResolver` trait to `AgentLoop` that generates the system prompt from live `SessionState` at the start of each turn. Allows prompts that adapt to context established earlier in the same conversation without a session restart.

#### 3.3 Voice pipeline

The core loop does not need to change. Only two adapters are needed: audio-in (STT → NATS event) and audio-out (text → TTS → WebSocket audio stream). Lower priority pending STT/TTS provider decisions.

---

### Summary table

| Feature | SDK | Trogonai | Recommendation |
|---|---|---|---|
| Agent execution loop | ✅ | ✅ | Already matched |
| MCP tool integration | ✅ | ✅ | Already matched |
| Streaming events | ✅ | ✅ | Already matched |
| Permission gating | ✅ | ✅ | Already matched |
| Prompt caching | ✅ | ✅ | Already matched |
| Context compaction | ✅ | ✅ | Already matched |
| Session persistence | ✅ | ✅ | Already matched |
| Distributed architecture | ❌ | ✅ | Trogonai ahead |
| ACP protocol | ❌ | ✅ | Trogonai ahead |
| Session forking | ❌ | ✅ | Trogonai ahead |
| Event sourcing / crash recovery | ❌ | ✅ | Trogonai ahead |
| Multi-transport (stdio/ws/nats) | ❌ | ✅ | Trogonai ahead |
| Extended thinking | ❌ | ✅ | Trogonai ahead |
| Multi-tenancy | ❌ | ✅ | Trogonai ahead |
| **Guardrails** | ✅ | ❌ | **Add — Priority 1** |
| **Tool use behavior control** | ✅ | ❌ | **Add — Priority 1** |
| **Lifecycle hooks** | ✅ | ❌ | **Add — Priority 1** |
| **Semantic trace spans** | ✅ | ❌ | **Add — Priority 1** |
| **Handoffs with input filter** | ✅ | ❌ | **Add — Priority 2** |
| **Agent-as-tool** | ✅ | ❌ | **Add — Priority 2** |
| **Structured output typing** | ✅ | ❌ | **Add — Priority 2** |
| **Session history windowing** | ✅ | ❌ | **Add — Priority 2** |
| RunState pause/resume | ✅ | Partial | Extend — Priority 3 |
| Dynamic instructions per-turn | ✅ | Partial | Extend — Priority 3 |
| Multiple session backends | ✅ | NATS KV only | Consider — Priority 3 |
| Voice pipeline | ✅ | ❌ | Future — Priority 3 |
