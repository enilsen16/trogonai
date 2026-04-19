# OpenAI Agents SDK — Technical Study

> **Repository:** https://github.com/openai/openai-agents-python  
> **Version at time of study:** 0.14.x  
> **Date:** 2026-04-19

---

## Table of Contents

1. [What It Is and Why It Exists](#1-what-it-is-and-why-it-exists)
2. [Core Architecture and Abstractions](#2-core-architecture-and-abstractions)
3. [The Agent Loop](#3-the-agent-loop)
4. [Tool System](#4-tool-system)
5. [Multi-Agent Orchestration Patterns](#5-multi-agent-orchestration-patterns)
6. [Guardrails](#6-guardrails)
7. [Sessions and Memory](#7-sessions-and-memory)
8. [Tracing and Observability](#8-tracing-and-observability)
9. [Streaming and Voice](#9-streaming-and-voice)
10. [Design Philosophy](#10-design-philosophy)
11. [Applicability to Trogonai](#11-applicability-to-trogonai)

---

## 1. What It Is and Why It Exists

The OpenAI Agents SDK is a production-grade Python framework for building multi-agent AI applications. It is the official successor to OpenAI's earlier research prototype Swarm and is actively used to power OpenAI's own agent products. With over 23,000 GitHub stars, 268 contributors, and 84 releases as of this writing, it represents a mature, battle-tested approach to agent orchestration.

### Problem space

Building AI agents in production is harder than calling an LLM once. Real workflows require:

- **Turn management** — the model must loop until it decides to stop or a tool returns a final result.
- **Tool execution** — function invocations must be dispatched, their results appended to the conversation, and the LLM re-invoked.
- **Multi-agent coordination** — complex tasks benefit from specialized agents collaborating, either in a hub-and-spoke topology or by peer delegation.
- **Safety** — outputs need validation before they reach users or downstream systems.
- **Durability** — long-running work should survive crashes and resume from checkpoints.
- **Observability** — understanding what happened inside a multi-step agentic run requires detailed structured tracing.
- **State persistence** — conversation history must be stored and retrieved efficiently, with pluggable backends to suit different deployment environments.

The SDK automates all of these concerns behind five core abstractions, allowing developers to focus on agent behavior rather than infrastructure plumbing.

---

## 2. Core Architecture and Abstractions

The entire framework is built on exactly five primitives. This minimalism is intentional and is one of the SDK's most important design choices.

### 2.1 Agent

An `Agent` is the central unit. It bundles:

| Field | Type | Description |
|---|---|---|
| `name` | `str` | Human-readable identifier |
| `instructions` | `str \| Callable` | Static string or a function that generates the system prompt at runtime |
| `tools` | `list[Tool]` | Any mix of tool types (see §4) |
| `handoffs` | `list[Handoff]` | Other agents this agent can delegate to |
| `guardrails` | `InputGuardrail / OutputGuardrail` | Safety hooks |
| `output_type` | `type` | Optional Pydantic model for structured responses |
| `model` | `str` | Model identifier; can be overridden per-run |
| `tool_use_behavior` | `enum / config` | Controls when the loop stops after a tool call |

The `Agent` is generic over a context type `TContext`, meaning a mutable context object can be threaded through all tool calls for state sharing within a run.

**Dynamic instructions** deserve special attention: instead of a static string, `instructions` can be an async callable that receives the run context at runtime. This enables prompt content that adapts to live state — user profiles, feature flags, loaded knowledge — without requiring separate loader machinery.

**`as_tool()`** is a powerful method that converts any `Agent` into a `FunctionTool`. The converted agent runs inside the parent agent's loop as a tool call, returns its final text output as the tool result, and never takes over the conversation. This is the foundation of the Manager pattern (see §5).

### 2.2 Runner

The `Runner` manages the agent execution loop. It is the only entry point for actually running an agent:

```python
result = await Runner.run(agent, input="...")          # async
result = Runner.run_sync(agent, input="...")           # sync wrapper
stream  = Runner.run_streamed(agent, input="...")      # streaming events
```

Internally, the runner:
1. Retrieves session history (if a session is configured).
2. Resolves `instructions` (calling the dynamic function if needed).
3. Assembles the full message list.
4. Invokes the LLM.
5. If `stop_reason == "tool_use"`: dispatches tools, appends results, loops back to step 4.
6. If `stop_reason == "end_turn"`: validates output (guardrails), saves session history, returns.
7. If a handoff tool was called: switches active agent and restarts the loop from step 2.

The runner also exposes `RunState` objects — serializable snapshots of execution state — enabling pause-and-resume workflows (e.g., awaiting human approval).

### 2.3 Tools

Covered in full in §4.

### 2.4 Handoffs

A `Handoff` is a first-class primitive (not just a function tool) representing a one-way delegation from one agent to another. When the LLM calls a handoff, the runner switches the active agent and continues the loop under the new agent's system prompt, with full conversation history preserved.

Key customization points:
- `tool_name_override` / `tool_description_override` — what the LLM sees.
- `on_handoff` — callback invoked when the handoff fires (for logging, side effects).
- `input_type` — a Pydantic model the LLM populates when triggering the handoff, enabling structured metadata to travel with the delegation.
- `input_filter` — a function that transforms the conversation history before it is passed to the receiving agent, preventing context explosion in long conversations.

### 2.5 Guardrails

Covered in full in §6.

---

## 3. The Agent Loop

Understanding the agent loop in detail is critical, because it is what makes the SDK production-ready rather than a toy wrapper.

```
┌──────────────────────────────────────────┐
│  Runner.run(agent, input)                │
│                                          │
│  1. Resolve instructions (dynamic/static)│
│  2. Retrieve session history             │
│  3. Run input guardrails                 │
│                                          │
│  ┌────────────────────────────────────┐  │
│  │  LOOP (max_turns)                  │  │
│  │                                    │  │
│  │  4. Call LLM → response            │  │
│  │                                    │  │
│  │  if stop_reason == end_turn:       │  │
│  │    run output guardrails           │  │
│  │    save session history → return   │  │
│  │                                    │  │
│  │  if tool_use:                      │  │
│  │    dispatch tools (parallel)       │  │
│  │    apply tool_use_behavior rules   │  │
│  │    append tool results → loop      │  │
│  │                                    │  │
│  │  if handoff tool called:           │  │
│  │    apply input_filter              │  │
│  │    switch active agent → restart   │  │
│  └────────────────────────────────────┘  │
│                                          │
│  MaxTurnsExceeded if loop limit hit      │
└──────────────────────────────────────────┘
```

### Tool use behavior

One of the SDK's more nuanced features is the `tool_use_behavior` field on `Agent`. It controls what the runner does after executing tool calls:

| Behavior | Description |
|---|---|
| `run_llm_again` (default) | Always re-invoke the LLM with tool results |
| `stop_on_first_tool` | Immediately return after the first tool call |
| `StopAtTools(["tool_name"])` | Return when any of the listed tools is called |
| custom callable | Developer-defined logic based on tool name + result |

This gives operators precise control over when the loop terminates, which is crucial for cost management and predictability in high-volume systems.

### RunState and pause/resume

`RunState` is a serializable snapshot of the entire execution at any point in the loop. A run can be paused before a tool executes (e.g., awaiting a human approval step) and resumed later with the same state. This enables deterministic human-in-the-loop workflows without external coordination.

---

## 4. Tool System

The SDK recognizes that tools come from fundamentally different execution environments and separates them into distinct categories rather than forcing a single abstraction.

### 4.1 Function Tools

Python functions decorated with `@function_tool`. The decorator automatically:
- Generates a JSON Schema from the function signature.
- Validates incoming parameters via Pydantic.
- Handles errors and returns them as string results to the LLM.
- Supports configurable per-tool timeouts.
- Supports rich returns (text + images/files in a single result).

```python
@function_tool
async def search_issues(query: str, project: str = "default") -> str:
    """Search for issues in the project tracker."""
    ...
```

### 4.2 Hosted OpenAI Tools

Tools that execute on OpenAI's infrastructure, requiring no local compute:
- `WebSearchTool` — live web search.
- `FileSearchTool` — retrieval from uploaded file corpora.
- `CodeInterpreterTool` — sandboxed code execution.
- `HostedMCPTool` — MCP servers running on OpenAI's infrastructure.

### 4.3 MCP Server Tools

Model Context Protocol (MCP) servers running locally or at a URL. The SDK discovers available tools via `list_tools()`, prefixes them to avoid collisions, and dispatches calls transparently alongside function tools.

### 4.4 Agents as Tools

Any agent can be converted to a function tool via `agent.as_tool()`. The parent agent retains control; the sub-agent runs to completion, and its final output becomes the tool result string. This is the foundation of the Manager orchestration pattern.

### 4.5 Computer and Shell Tools

`ComputerTool` and `ShellTool` enable GUI automation and command execution respectively. `ApplyPatchTool` applies unified diffs.

---

## 5. Multi-Agent Orchestration Patterns

### 5.1 The Handoff Pattern (peer delegation)

A routing agent delegates to specialists. The specialist takes over the conversation, has access to full history, and can in turn hand off to another agent. Control flows sequentially from agent to agent.

```
User → Triage Agent
             │
             ├── handoff → Support Agent
             ├── handoff → Sales Agent
             └── handoff → Billing Agent
```

**Best for:** customer-facing workflows where conversation focus is important, workflows where each stage has a clearly different system prompt and tool set.

**Key advantage:** conversation history is preserved automatically through handoffs. Input filters can trim it before passing to the next agent, preventing context windows from growing unboundedly.

### 5.2 The Manager Pattern (agents as tools)

A central orchestrator agent invokes specialists as tools. Specialists do not take over — they run to completion and return their output as a string. The manager synthesizes results and maintains full control.

```
Manager Agent
   │
   ├── tool call → Specialist A (runs as sub-agent, returns text)
   ├── tool call → Specialist B (runs in parallel via asyncio.gather)
   └── synthesizes → final response
```

**Best for:** tasks requiring multiple specialist outputs to be combined, scenarios needing unified guardrails at the top level, parallel independent sub-tasks.

### 5.3 Code-Based Orchestration

Beyond LLM-directed orchestration, the SDK is designed to work alongside regular Python code. Patterns include:

- **Sequential chaining**: transform one agent's structured output into the next agent's input using `output_type` (Pydantic).
- **Conditional routing**: inspect a structured `output_type` field to decide which agent runs next.
- **Parallel execution**: `asyncio.gather([Runner.run(agent_a, ...), Runner.run(agent_b, ...)])`.
- **Feedback loops**: an evaluator agent reviews an output and feeds corrections back.

The framework's explicit position is that code-based orchestration is more predictable and cheaper than LLM-directed orchestration, and should be preferred for deterministic routing decisions.

---

## 6. Guardrails

Guardrails are validation functions that can tripwire — immediately halt execution and raise a typed exception — based on input or output content.

### Three levels

| Level | When it runs | Scope |
|---|---|---|
| **Input guardrail** | Before the first LLM call | Only on the first agent in a chain |
| **Output guardrail** | After the final LLM call returns | Only on the last agent in a chain |
| **Tool guardrail** | Before/after each `@function_tool` invocation | Per-tool |

Input guardrails support two execution modes:
- **Parallel** (default): runs concurrently with the LLM call. Faster; may consume tokens before the tripwire fires.
- **Blocking**: completes before the LLM is invoked. Prevents unnecessary LLM spend.

Guardrail functions return a `GuardrailFunctionOutput` with a boolean `tripwire_triggered`. When `True`, the runner raises `InputGuardrailTripwireTriggered` or `OutputGuardrailTripwireTriggered`, which propagate up to the application layer.

### Design rationale

Separating guardrails by level is a deliberate cost-optimization choice. An input guardrail can prevent an entire expensive multi-turn run from starting. An output guardrail validates only after the LLM has done its work, so it should be cheap. Tool guardrails protect side-effecting functions independently of agent boundaries.

---

## 7. Sessions and Memory

### Automatic conversation history

Sessions are attached to a `Runner` call via `RunConfig`. When a session is provided, the runner:
1. Calls `session.get_items()` before each run and prepends history to the input.
2. Calls `session.add_items()` after each run with all new messages.

The developer writes no memory code. History management is fully automated.

### Pluggable backends

Six session backends are available out of the box:

| Backend | Best for |
|---|---|
| `SQLiteSession` | Development, single-machine, lightweight |
| `RedisSession` | Distributed, horizontally scaled deployments |
| `SQLAlchemySession` | PostgreSQL/MySQL, production relational databases |
| `DaprSession` | Cloud-native (supports 30+ state store providers) |
| `OpenAIConversationsSession` | OpenAI-managed storage, minimal ops overhead |
| `EncryptedSession` | Transparent AES-256 encryption wrapper over any backend |

All backends implement the same interface. Swapping backends is a one-line change in `RunConfig`.

### History management controls

- `SessionSettings(limit=N)` — retrieve only the N most recent turns. Prevents unbounded context growth.
- `RunConfig.session_input_callback` — custom function to merge retrieved history with new input. Enables summarization, compression, or selective filtering before the LLM sees history.
- Input filters on handoffs — transform the conversation before passing to the next agent (trim tool results, summarize long exchanges, remove sensitive blocks).

---

## 8. Tracing and Observability

Tracing is enabled by default for every run. No configuration is required to get complete span coverage.

### Span types

The framework automatically creates spans for:

| Span type | What it covers |
|---|---|
| `AgentSpan` | Full agent execution from start to finish |
| `GenerationSpan` | Each individual LLM API call |
| `ToolCallSpan` | Each tool invocation with name and input |
| `HandoffSpan` | Each agent delegation |
| `GuardrailSpan` | Each guardrail evaluation |
| `TurnSpan` | Complete single turn (user input → assistant output) |
| `TaskSpan` | Custom user-defined operations |

Spans are hierarchical: a `TurnSpan` contains `GenerationSpan`s and `ToolCallSpan`s nested inside it.

### Processor model

Trace data flows through a configurable processor pipeline:

```python
add_trace_processor(MyExporter())       # add alongside default
set_trace_processors([MyExporter()])    # replace all defaults
```

This decouples the framework from any specific observability backend. Built-in integrations exist for:
Weights & Biases, Arize-Phoenix, MLflow, Braintrust, LangSmith, Langfuse, and 18 others.

### Configuration

```python
RunConfig(
    tracing_disabled=True,                  # disable for a specific run
    trace_include_sensitive_data=False,     # suppress message content in traces
)
```

`flush_traces()` is available for long-running processes that need immediate export without waiting for the background flush interval.

---

## 9. Streaming and Voice

### Text streaming

`Runner.run_streamed()` returns a `RunResultStreaming` object that emits semantic events (not raw tokens). Event types include:

- `agent_updated` — active agent changed (handoff occurred)
- `tool_called` / `tool_output` — tool lifecycle
- `raw_response_event` — direct LLM stream chunk
- `run_item_stream_event` — completed message blocks

### Voice pipeline

The voice architecture is a three-stage pipeline:

```
Microphone → [Speech-to-Text] → [Agent Workflow] → [Text-to-Speech] → Speaker
```

Two input modes:
- `AudioInput` — full audio clip, transcribed before agent runs (push-to-talk).
- `StreamedAudioInput` — continuous stream with automatic voice activity detection; transcription happens in real time.

Output is a `StreamedAudioResult` emitting:
- Audio chunk events (PCM bytes).
- Lifecycle events (`turn_started`, `turn_ended`).
- Error events.

The voice pipeline uses the exact same agent primitives (tools, handoffs, guardrails, sessions). There is no separate "voice agent" concept — any existing agent can be plugged into a `VoicePipeline` without modification.

The TypeScript version of the SDK offers an alternative `RealtimeSession` path that uses OpenAI's real-time speech-to-speech API, bypassing the STT→agent→TTS chain for lower latency.

---

## 10. Design Philosophy

Several explicit design principles run through the codebase and documentation.

### Minimal, composable primitives

Five abstractions — Agent, Runner, Tools, Handoffs, Guardrails — cover all use cases. The SDK avoids inventing new concepts when Python language features or simple function composition suffice. Contrast with frameworks like LangGraph (graph state machines) or CrewAI (role/task ontologies), which introduce heavier conceptual models.

### Code-first orchestration

The SDK is explicit that LLM-directed orchestration (agents deciding which tool or handoff to invoke) is more flexible but less predictable and more expensive than code-directed orchestration (regular Python logic deciding which agent to call next). It supports both equally and makes no recommendation that all routing must go through an LLM.

### Production defaults

Tracing is on by default. Retry logic is on by default. Session persistence works without configuration if a backend is provided. The design assumes the user is building production software, not a demo.

### Observability as infrastructure

Tracing is not an afterthought added after the core was built. Span types map exactly to the conceptual operations (generation, tool call, handoff, guardrail), which means traces are semantically meaningful to humans reading them, not just raw call graphs.

### Tool source diversity

The SDK explicitly models that tools come from four structurally different sources: local functions, OpenAI-hosted services, MCP servers, and other agents. Each is treated with its own dispatch logic rather than forcing them all through a single interface.

---

## 11. Applicability to Trogonai

Trogonai is a production-grade Rust agent orchestration platform with a sophisticated architecture: NATS JetStream-based durability (promise store for crash recovery), skill injection from NATS KV, dynamic automation routing, MCP integration, multi-tenancy, OpenTelemetry tracing, and a secret proxy that prevents real API keys from reaching agent processes.

The following analysis maps OpenAI SDK concepts to the trogonai codebase and identifies concrete gaps or improvements.

---

### 11.1 Guardrails — Not present in trogonai

**SDK:** Three-level (input, output, tool) validation with tripwire semantics. Input guardrails prevent expensive runs before they start. Output guardrails validate before results reach the application layer. Tool guardrails wrap individual `@function_tool` functions.

**Trogonai today:** No validation layer exists between event ingestion and the LLM, between the LLM output and the NATS reply, or around individual tool calls. The `agent_loop.rs` dispatches tools and returns LLM output without any policy enforcement point.

**Recommendation:** Add a `GuardrailHook` trait with `before_run` and `after_run` async methods. Wire it into `AgentLoop::run()` at the appropriate points. Input hooks can reject malformed or policy-violating events before tokens are spent. Output hooks can detect hallucinations, PII leakage, or off-topic responses before they propagate downstream. This is especially important as trogonai adds more tenants and automations with unpredictable prompts.

```rust
#[async_trait]
pub trait GuardrailHook: Send + Sync {
    async fn on_input(&self, prompt: &str, ctx: &RunContext) -> GuardrailResult;
    async fn on_output(&self, output: &str, ctx: &RunContext) -> GuardrailResult;
}

pub enum GuardrailResult {
    Pass,
    Block { reason: String },
}
```

---

### 11.2 Handoffs as a First-Class Primitive — Partially covered

**SDK:** Handoffs carry full conversation history, support input filters to trim it, and support structured metadata (`input_type`) that the LLM populates when delegating. They are distinct from regular tool calls.

**Trogonai today:** The `Automation.trigger` field routes events to the correct automation, but routing happens in `runner.rs` in application code before the LLM is invoked. There is no mechanism for the LLM itself to decide mid-run to delegate to a different agent with the current conversation state. The promise store ties a run to one agent definition for its entire lifetime.

**Recommendation:** Add a `HandoffTool` variant to the tool dispatch path. When the LLM calls a handoff tool, the loop pauses, the current `AgentPromise` is transferred to the target agent's KV namespace, and execution resumes under the new agent definition and skill set. This enables the LLM to autonomously route complex multi-stage tasks (e.g., a triage agent handing off to a specialized incident handler mid-conversation).

The most valuable sub-feature to port first is the **input filter** — a function that trims or summarizes the message history before it is passed to the receiving agent. Trogonai's promises currently carry the full raw message history; in long conversations this pushes against the 768 KB checkpoint size limit, and reduces accuracy as irrelevant context dilutes the model's attention.

---

### 11.3 RunState / Human-in-the-Loop Pause-Resume — Not present

**SDK:** `RunState` is a serializable snapshot of a mid-loop execution. A run can be paused before a tool executes (e.g., the tool requires human approval), persisted, and resumed later by restoring the state. Used for approval workflows and human-supervised actions.

**Trogonai today:** The promise store (`AgentPromise`) already persists full message history across crashes. What it does not support is an intentional paused state with a structured wait for external input. The `PromiseStatus` enum has `Running`, `Resolved`, `PermanentFailed` — no `AwaitingApproval` or `Suspended` state.

**Recommendation:** This is close to the existing durability infrastructure. Adding a `Suspended` status and a NATS request-reply subject that a human (or another process) can ACK would enable human-in-the-loop without rebuilding the checkpoint machinery. The agent pauses before executing a high-risk tool (e.g., `update_file`, `create_pull_request`), writes a `Suspended` promise, sends a NATS message requesting approval, and resumes only when the approval reply arrives.

---

### 11.4 Tool Use Behavior Control — Not present

**SDK:** The `tool_use_behavior` field controls when the loop terminates relative to tool calls: always re-invoke LLM, stop immediately after first tool, stop when specific named tools are called.

**Trogonai today:** `agent_loop.rs` always re-invokes the LLM after tool results until `stop_reason == "end_turn"` or `max_iterations` is hit. There is no way to configure "return immediately after this tool reports success" without modifying Rust source.

**Recommendation:** Add a `LoopStopPolicy` enum to `AgentDefinition` in the console:

```rust
pub enum LoopStopPolicy {
    AlwaysContinue,           // current behavior
    StopOnFirstTool,
    StopAtTools(Vec<String>), // tool names that trigger immediate return
}
```

This is particularly useful for automations where the goal is exactly one tool call (e.g., post a Slack message, create a Linear issue) and the rest of the loop is wasted tokens.

---

### 11.5 Structured Output Types for Code-Based Routing — Not present

**SDK:** An agent can declare `output_type = SomePydanticModel`. When it does, the runner validates the final response against that schema using structured output mode. Application code can then branch on fields of that model without asking another LLM.

**Trogonai today:** All agent outputs are unstructured strings. Automation routing decisions (which automation to trigger) are made pre-LLM based on event subject patterns. There is no way for the LLM to return structured data that drives subsequent code-level decisions.

**Recommendation:** The Anthropic API supports a `type: "json"` tool pattern that can serve the same purpose. Adding an optional `output_schema: serde_json::Value` field to `AgentDefinition` would allow the last LLM call in a loop to produce validated JSON. Consuming automations or the console API could inspect structured fields (severity, category, recommended_action) without another model call. This reduces latency and cost on categorization and triage workloads.

---

### 11.6 Session History Management Controls — Partially covered

**SDK:** `SessionSettings(limit=N)` caps history to N turns. `RunConfig.session_input_callback` customizes how stored history merges with new input (enabling summarization or compression).

**Trogonai today:** `ChatSession.messages` in `session.rs` stores the full history. The 768 KB checkpoint limit in the promise store is the only enforced bound, and it triggers a hard error rather than a graceful trim. Interactive sessions have no windowing.

**Recommendation:** Add a `max_turns: Option<usize>` to `ChatSession` and apply a sliding-window trim when loading history in `chat_api.rs`. For the promise store, add a summarization step: when the serialized promise exceeds a configurable threshold (say 512 KB), invoke a cheap fast model to summarize older exchanges and replace them with the summary block before writing the checkpoint. This is preferable to failing with a size error.

---

### 11.7 Dynamic Instructions — Already present, worth formalizing

**SDK:** `instructions` can be a callable that receives the current run context and returns a string. This enables live-reloaded system prompts without restart.

**Trogonai today:** `agent_loader.rs` fetches the `AgentDefinition` from `CONSOLE_AGENTS` KV on every turn with no caching, which is architecturally equivalent to dynamic instructions. `skill_loader.rs` similarly fetches and injects skills fresh per turn. This is a strong design.

**Note:** The trogonai approach is arguably better than the SDK's callable approach: it decouples the prompt from the process binary entirely, allowing non-engineers to update agent behavior at runtime through the console UI without any code change or restart. This design decision is sound and should be preserved and documented explicitly as an intentional architecture choice.

---

### 11.8 Trace Processor Pipeline — Partially covered

**SDK:** `add_trace_processor()` / `set_trace_processors()` for a pluggable exporter pipeline. Automatic spans for LLM calls, tool calls, handoffs, guardrail evaluations.

**Trogonai today:** `acp-telemetry` sets up OpenTelemetry with OTLP export. The `trogon-nats` crate propagates trace context in NATS headers. `agent_loop.rs` logs key events via `tracing::`. However, there are no structured domain-specific span types: there is no `ToolCallSpan` with input/output, no `HandoffSpan`, no `GuardrailSpan`. Observability exists at the infrastructure level but not at the agentic semantic level.

**Recommendation:** Add semantic span types that mirror the SDK's taxonomy, emitted at key points in `agent_loop.rs`:

```rust
span!("tool_call", tool.name = %name, tool.input = %input_json);
span!("llm_generation", model = %model, tokens.in = %usage.input_tokens, tokens.out = %usage.output_tokens, cache.hit = %cache_read_tokens);
span!("agent_run", agent.id = %agent_id, automation.id = %automation_id, promise.id = %promise_id);
```

This makes traces actionable for debugging — you can see exactly which tool was called with which input, which LLM call consumed how many tokens, and which agent handled which event.

---

### 11.9 Manager Pattern — Not present

**SDK:** A central orchestrator invokes specialists as tools via `agent.as_tool()`. Specialists run in parallel with `asyncio.gather`.

**Trogonai today:** Automation dispatch is parallel (multiple automations fire concurrently for the same event), but each automation runs independently — there is no shared supervisor agent synthesizing their outputs. There is no concept of one agent calling another agent mid-loop.

**Recommendation:** This requires the `as_tool()` equivalent: a mechanism for an agent's tool call to spin up a sub-agent run (with its own promise, skill set, and system prompt), wait for it to complete, and return the result as a tool result string. In NATS terms: when a special `invoke_agent` tool is called, the loop publishes a sub-run event, subscribes to its completion reply, and blocks with a timeout. This unlocks complex hierarchical orchestration (e.g., an incident commander agent dispatching diagnostic sub-agents to different services simultaneously).

---

### 11.10 Voice Pipeline — Not present

**SDK:** Native STT → agent → TTS pipeline. Any agent works as-is inside a `VoicePipeline`.

**Trogonai today:** No voice support exists.

**Recommendation:** Voice is a longer-term concern but worth designing for from the start. The architectural requirement is an async streaming audio pipeline:

```
WebSocket (audio in) → STT service (e.g., Deepgram, Whisper) → NATS event → agent_loop → TTS service → WebSocket (audio out)
```

Since trogonai's agent_loop already handles text in/out, the only additions needed are audio I/O adapters and a voice-aware session store that persists transcripts. The core loop does not need to change.

---

### Summary Table

| SDK Feature | Trogonai Status | Priority |
|---|---|---|
| Guardrails (input/output/tool) | Not present | High |
| Handoffs with input filters | Not present | High |
| Structured output types | Not present | Medium |
| Tool use behavior control | Not present | Medium |
| RunState / human-in-the-loop pause | Not present | Medium |
| Session history windowing | Partial (size limit only) | Medium |
| Dynamic instructions | Present (via agent_loader + skill_loader) | ✓ Already done |
| Semantic trace spans | Partial (infra-level only) | Medium |
| Manager pattern (agent-as-tool) | Not present | Low/Medium |
| Multiple session backends | Not present (NATS KV only) | Low |
| Voice pipeline | Not present | Low |
| Crash recovery / durability | Present (promise store) | ✓ Ahead of SDK |
| Multi-tenancy | Present | ✓ Ahead of SDK |
| Secret proxy / token security | Present | ✓ Ahead of SDK |
| Live skill/agent reload | Present | ✓ Ahead of SDK |
| Feature-flag gating | Present (Split.io) | ✓ Ahead of SDK |

---

### Where Trogonai is architecturally ahead

It is worth noting that trogonai's design solves several problems the OpenAI SDK leaves to the application developer:

- **Crash recovery with idempotent tool replay** — The promise store + SHA-256 tool result cache is significantly more robust than anything the Python SDK offers. The SDK has no crash recovery; trogonai can resume a run that died mid-loop across machine reboots.
- **Token security via secret proxy** — Real API keys never reach agent processes. The Python SDK assumes keys are present in the environment.
- **Live skill and agent reload** — System prompts and skill sets are updated in NATS KV and take effect on the next turn without any restart. The Python SDK requires redeploying code.
- **Built-in multi-tenancy** — All KV namespacing, feature flag evaluation, and consumer isolation are tenant-aware. The SDK has no multi-tenancy concept.
- **NATS-based durability for all state** — Promises, sessions, automations, skills, and agent definitions all live in JetStream, enabling replication, persistence, and cluster-wide consistency without additional infrastructure.
