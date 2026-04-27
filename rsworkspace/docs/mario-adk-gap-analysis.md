# Trogonai vs Google Gemini Enterprise Agent Platform
## API & Capabilities Gap Analysis

**Date:** 2026-04-27
**Source:** https://docs.cloud.google.com/gemini-enterprise-agent-platform/build/adk

---

## Structure of This Document

The Gemini Enterprise Agent Platform has four layers:

- **Build** — author agents: ADK framework, A2A protocol, Agent Garden, Agent Studio, RAG Engine, Vector Search, Authentication
- **Scale** — run agents: Agent Runtime, Sessions, Memory Bank, Code Execution
- **Govern** — control agents: Safety, Semantic Governance Policies, Agent Registry, Agent Gateway, Model Armor
- **Optimize** — improve agents: Evaluation, Observability/Topology, Example Store

For each section this document describes what the platform API provides, what a developer can call or configure, and how Trogonai compares at that capability level.

---

## SECTION 1: BUILD

### 1.1 Agent Definition — `LlmAgent`

The core authoring primitive of ADK is `LlmAgent`. A developer declares a complete agent in a single constructor call, passing parameters for the model name, a display name, a natural-language instruction, a list of tools, optional input and output schemas (Pydantic in Python, Zod in TypeScript), an output key that saves the final response into session state, a planner strategy, and a code executor. The same constructor API is available identically in Python, TypeScript, Go, and Java — switching language requires no conceptual change.

**Trogonai:** There is no `LlmAgent` or equivalent declarative API. An agent is a Rust binary that implements the ACP `Agent` trait or the `EntityActor` pattern. There is no `model` parameter, no `output_schema`, no `planner`, and no multi-language support. Authoring requires writing and compiling Rust.

**Gap:** Authoring a Trogonai agent requires Rust expertise. The platform reduces it to a single constructor call in four languages.

---

### 1.2 Workflow Agents — Sequential, Parallel, Loop

ADK provides three deterministic composition primitives that orchestrate multiple agents without involving an LLM in the control flow.

`SequentialAgent` runs sub-agents one after another in a fixed order, passing state between them. Each agent can write a result into a named key in session state and the next agent can reference that key in its instruction template, creating a typed data pipeline. The canonical example is a three-step pipeline where one agent writes code, a second reviews it, and a third refactors it using the review output.

`ParallelAgent` runs sub-agents concurrently. There is no automatic state sharing between concurrent branches — each agent stores its result via its own output key and a downstream agent aggregates them.

`LoopAgent` repeats a list of sub-agents up to a configurable maximum number of iterations. A sub-agent can signal early termination by setting a flag on the tool context. The typical use case is an iterative refinement loop where a generator and a reviewer alternate until quality is met or the iteration limit is reached.

All three constructors are available identically in Python, TypeScript, Go, and Java.

**Trogonai:** `SequentialAgent`, `ParallelAgent`, and `LoopAgent` do not exist. Sequential steps must be hard-coded inside an actor or chained manually via NATS messages. Parallel execution requires publishing to multiple subjects and aggregating replies by hand. `trogon-automations` handles trigger-based flows but is not a general pipeline primitive.

**Gap:** Deterministic pipeline composition does not exist in Trogonai as a developer API.

---

### 1.3 Custom Agents — `BaseAgent`

When `LlmAgent` is not sufficient, developers subclass `BaseAgent` and implement a single async generator method that receives the invocation context and yields typed events. `BaseAgent` provides the sub-agents list, a `find_agent(name)` lookup, and the parent agent reference set automatically by the framework. All lifecycle callbacks are inherited.

**Trogonai:** `EntityActor` in `trogon-actor` is functionally the closest equivalent — a custom handler processing NATS messages with persistent state in NATS KV. It is Rust-only and does not follow an event generator pattern.

---

### 1.4 Tool Definition

ADK supports five tool authoring mechanisms:

**Function tools** — the developer writes an annotated function with typed parameters and a docstring. ADK auto-generates the LLM tool schema from the type hints and docstring. No additional configuration is needed. The same mechanism works in TypeScript with Zod schemas, Go with a function wrapper, and Java with a builder.

**`LongRunningFunctionTool`** — wraps any function that produces intermediate results or requires waiting (for example, requesting human approval). The agent continues to receive status updates during execution.

**`AgentTool`** — wraps any existing agent as a callable tool, allowing a parent LLM to invoke it exactly like a function. This enables recursive agent composition without NATS subject routing.

**OpenAPI tools** — ADK reads any OpenAPI or Swagger specification and auto-generates a fully functional tool set. No manual implementation is required.

**MCP tools** — `McpToolset` connects to any MCP server, either as a local subprocess or a remote HTTP server. It handles the full connection lifecycle, calls `list_tools` to discover available tools, adapts their schemas to ADK format, and dispatches `call_tool` requests. A tool filter parameter lets the agent expose only a subset of the server's tools.

**Trogonai:** Tools are available via MCP only through `trogon-mcp`. There is no inline function tool, no `AgentTool`, no `LongRunningFunctionTool`, and no OpenAPI generation. Human-in-the-loop exists only in `trogon-vault-approvals` for credential write approvals.

**Gap:** Every Trogonai tool requires a running MCP server process. ADK supports five-line inline definition and OpenAPI auto-generation.

---

### 1.5 Callbacks — Runtime Interception Hooks

ADK provides six hooks that fire at precise execution points. Returning a value from any hook short-circuits the default behavior, giving the developer full control over what the agent sees and produces.

`before_agent_callback` fires before the agent starts. Returning content from this hook skips the agent entirely — useful for caching or fast-path responses. `after_agent_callback` fires after the agent finishes and can replace the output.

`before_model_callback` fires before every LLM call, receiving the full request. Returning an `LlmResponse` object from this hook skips the model call. `after_model_callback` fires after the model responds and can sanitize or replace the output — the primary hook for PII redaction and output filtering.

`before_tool_callback` fires before a tool executes, receiving the tool input. Returning a dict from this hook skips the tool call — the primary hook for access control. `after_tool_callback` fires after the tool completes and can replace the result.

All six hooks can read and write session state and have access to the full context hierarchy.

**Trogonai:** No callback system exists. Safety, PII redaction, and policy enforcement must be reimplemented manually inside every actor independently.

**Gap:** Callbacks are the primary mechanism for safety enforcement and governance. Their absence makes cross-cutting concerns expensive to implement consistently across agents.

---

### 1.6 Context APIs

ADK provides a typed context hierarchy that controls what each component can access at runtime.

`ReadonlyContext` is the base — it exposes the invocation ID, agent name, and read-only session state. `CallbackContext` extends it with read/write state access, artifact loading and saving, and the user's input content. `ToolContext` extends that further with memory search, credential request and retrieval, artifact listing, and the function call ID. `InvocationContext` is the full context available to the agent's core loop — it has access to the session object, all registered services, and the end-invocation flag.

The credential APIs on `ToolContext` are particularly notable: `request_credential(auth_config)` initiates an OAuth or API key consent flow inside a running tool, and `get_auth_response(auth_config)` retrieves the credential after the user completes consent.

**Trogonai:** `ActorContext` provides state access via NATS KV and a `spawn_agent()` call. MCP tools are pure JSON-in/JSON-out with no context injection. There is no `ToolContext`, no artifact access in tools, no in-tool memory search, and no credential request flow.

**Gap:** Tools in Trogonai have no access to session context, memory, or artifacts.

---

### 1.7 Session Management

ADK exposes session management through both an SDK interface and a direct REST API.

The SDK interface provides `create_session`, `get_session`, `list_sessions`, `append_event`, and `delete_session` methods. Three backend implementations are available: `InMemorySessionService` for development, `DatabaseSessionService` backed by async SQLite or Postgres, and `VertexAiSessionService` which is fully managed. Sessions carry a user ID, optional initial state, a TTL (default 365 days), and an optional absolute expiry time.

The REST API operates directly on the managed Agent Runtime without requiring the ADK SDK. Callers can create sessions with a POST request, retrieve them by ID, delete them, list all sessions for a user, retrieve the event history for a session, and append new events. Each event carries an author, an invocation ID, a timestamp, and structured content. This REST surface means any language or service can manage sessions without the ADK library.

**Trogonai:** Sessions are managed internally by the ACP runner through protocol messages — `new_session`, `fork_session`, `load_session`, `close_session`. There is no `SessionService` interface for developers to call and no REST API for sessions. Session state lives in actor state in NATS KV. External callers cannot manage sessions without speaking the raw NATS/ACP protocol.

**Gap:** Sessions are a protocol concept in Trogonai, not a developer API.

---

### 1.8 Memory — Memory Bank

ADK's Memory Bank provides persistent, cross-session memory with semantic vector search.

The SDK interface allows developers to add a completed session to memory with `add_session_to_memory` and to query memory with `search_memory`, which returns semantically ranked snippets relevant to the query. Inside a tool, `tool_context.search_memory(query)` retrieves facts from prior sessions. Two built-in tools handle memory injection automatically: `PreloadMemoryTool` auto-retrieves relevant memories at the start of every turn, and `LoadMemoryTool` lets the agent trigger retrieval on demand.

The `GenerateMemories` operation is a long-running REST call that extracts structured facts from a session or raw events. It accepts a data source (a session resource name, raw event lists, or pre-extracted memory objects), a scope that determines which existing memories are eligible for consolidation, an allowed-topics filter, metadata with a merge strategy (merge, overwrite, or require exact match), and a flag to disable deduplication. The response reports each memory as created, updated, or deleted.

Additional sub-APIs include `fetch-memories` to retrieve all memories for a user scope, `profiles` for aggregated per-user fact profiles, `revisions` for the history of how a memory evolved over time, and `ingest-events` to stream conversation events for asynchronous memory generation.

Two implementations are available: `InMemoryMemoryService` for keyword-matching during prototyping, and `VertexAiMemoryBankService` for semantic vector search in production.

**Trogonai:** `trogon-transcript` is an append-only audit log with no search. `trogon-compactor` manages in-context token budgets but does not retrieve knowledge across sessions. There is no `GenerateMemories`, no `search_memory`, no `PreloadMemoryTool`, and no vector store.

**Gap:** Trogonai agents have no long-term memory. Every session starts from zero.

---

### 1.9 Artifact Management

ADK provides versioned binary object storage scoped to either a session or a user.

Developers call `save_artifact` with a filename and a typed Part object (binary data with a MIME type); the method returns the version number assigned. `load_artifact` retrieves the latest version by default, or a specific version by number. `list_artifacts` returns the filenames available in scope. `list_versions` returns the full version history for a filename. Prefixing the filename with `user:` makes the artifact accessible across all sessions for that user rather than scoped to the current session.

Two backend implementations are available: `InMemoryArtifactService` for ephemeral use during testing, and `GcsArtifactService` backed by Google Cloud Storage for production.

**Trogonai:** No artifact system exists. Binary data management is not a platform capability.

**Gap:** No equivalent in Trogonai.

---

### 1.10 Running an Agent — The Runner

The `Runner` ties together an agent, a session service, an artifact service, and a memory service into a single callable unit. Developers call `run_async` with a user ID, session ID, and a new message; it returns an async event stream. Each event in the stream has a type — tool call, tool result, state delta, agent transfer, or model response — and a flag indicating whether it is the final response.

For local development, `adk web` launches a browser UI with a trace viewer that shows every event, request, and response in a visual graph. `adk run` executes an agent from the command line. `adk eval` runs an evaluation suite. None of these require any infrastructure setup.

**Trogonai:** There is no `Runner`. Execution is reactive NATS pub/sub. There is no typed event stream, no local dev server, and no browser trace viewer. Running any agent requires a running NATS cluster with correct subject routing configured.

**Gap:** Running a Trogonai agent requires infrastructure knowledge. ADK provides zero-configuration local execution.

---

### 1.11 Agent2Agent (A2A) Protocol

A2A is an open standard at a2a-protocol.org for cross-framework agent interoperability. An ADK agent, a LangGraph agent, a CrewAI agent, and a custom agent can all call each other using A2A regardless of which framework they were built with.

To expose an agent via A2A, the agent serves an `AgentCard` at a well-known URL describing its capabilities, and accepts task requests via a standard HTTP endpoint. To consume another agent, an `A2AAgent` is constructed with the remote agent's URL and can be used as a sub-agent inside any `LlmAgent`. Protocol operations include sending a task, polling for a result, and streaming responses. Model Armor screens A2A traffic at the Agent Gateway.

Available in Python, Go, and Java. Can be deployed on Agent Runtime and called from Agent Runtime.

**Trogonai:** No A2A protocol. Inter-agent communication uses NATS subjects, which works within the Trogonai ecosystem but is not interoperable with external agent frameworks.

**Gap:** Trogonai is a closed ecosystem for agent communication.

---

### 1.12 RAG Engine

The RAG Engine is a fully managed, six-step retrieval-augmented generation pipeline.

Step one is ingestion: documents are loaded from local files, Cloud Storage, or Google Drive. Step two is transformation: documents are parsed and chunked using semantic, fixed-size, hierarchical, layout-aware, or LLM-based chunking strategies. Step three is embedding: chunks are embedded using Vertex AI embeddings, Gemini embeddings, or a custom model. Step four is indexing: a RAG corpus is constructed as a structured index over the embedded chunks. Step five is retrieval: queries use vector similarity search, hybrid dense-plus-sparse search, or a combination. Step six is generation: retrieved context is injected into the LLM prompt, with source attribution included in the response.

Vector database backends include the managed RAG DB, Vertex AI Vector Search, Feature Store, Weaviate, and Pinecone. Deployment modes include Spanner-backed for high throughput and Serverless for zero configuration. Multimodal RAG is supported for text and images. CMEK encryption is supported across 21 regions.

Google Search grounding is also available as a built-in tool that performs real-time web retrieval at inference time. The Agent Garden provides a pre-built RAG agent template with a debug panel showing retrieval steps and reasoning.

**Trogonai:** No RAG pipeline, no document ingestion, no vector search, no hybrid retrieval, no grounding, no source attribution.

**Gap:** Knowledge-intensive agents cannot be built on Trogonai without implementing a complete RAG pipeline externally.

---

### 1.13 Vector Search 2.0

Vector Search 2.0 is a collections-based architecture that replaces the original index-as-a-service model. Three core concepts define it.

A Collection is a container of related JSON objects with an associated schema that can enforce strict or relaxed validation. A Data Object is an individual JSON item stored in a collection. A Collection Index enables approximate nearest neighbor search across a collection; multiple indexes can exist per collection, one per vector field.

Key improvements over Vector Search 1.0 include auto-tuning that eliminates manual VM and replica configuration, built-in embedding that auto-populates vector fields using integrated models, support for bringing your own embeddings, rich queries that combine vector similarity with payload filtering in a single request, unified storage that handles retrieval, filtering, and document management together, and two pricing models — usage-based and resource-based.

**Trogonai:** No vector search, no collections-based storage, and no ANN index exist at any level.

**Gap:** Semantic retrieval does not exist in Trogonai.

---

### 1.14 Authentication

ADK supports three credential mechanisms for agents calling external services.

API keys are static credentials injected via `request_credential` on the tool context. Two-legged OAuth covers service-to-service authentication using the client credentials flow — the agent authenticates as itself. Three-legged OAuth covers user-delegated access using the authorization code flow — `request_credential` initiates the consent flow inside a running tool and `get_auth_response` retrieves the token after the user completes consent.

The Auth Manager in the Agent Registry binds discovered tools to their required credential schemes. When an agent discovers a tool from the registry at runtime, it automatically receives the correct credential binding without any manual configuration.

**Trogonai:** The token proxy (`trogon-secret-proxy`) isolates static API keys — the agent holds an opaque token and the proxy resolves it to the real credential. This provides stronger isolation than IAM environment variables for static keys. Three-legged OAuth and dynamic auth binding for discovered tools do not exist.

**Partial parity / Gap:** Trogonai excels at static key isolation. 3-legged OAuth and dynamic auth binding are not present.

---

### 1.15 Agent Studio and Agent Garden

Agent Studio is a visual no-code and low-code agent design interface. Non-developers use it to compose agents, configure tools, set instructions, and test interactively in a browser. Agents designed in Agent Studio can be deployed directly to Agent Runtime without writing code.

Agent Garden is a curated library of pre-built agent templates with direct access to the GitHub source. Templates include ReAct, RAG, and multi-agent patterns. Each template provides immediate working functionality, an interactive chat test interface, a debug panel, and full customization via the source repository.

**Trogonai:** No visual design UI and no pre-built templates exist. Every agent is built from scratch in Rust.

**Gap:** Non-developer access to the platform does not exist in Trogonai.

---

## SECTION 2: SCALE

### 2.1 Agent Runtime (Reasoning Engine)

Agent Runtime is a fully managed, auto-scaling runtime built on the `ReasoningEngine` API resource. Deploying an agent requires a single CLI command that packages the agent directory and pushes it to the platform. Querying a deployed agent is done via a standard REST POST call to the agent's endpoint, or through the Python SDK by getting the engine resource and calling its query method.

The platform manages sessions, memory, scaling, monitoring, and availability automatically. Supported frameworks for deployment include ADK, LangChain, LangGraph, AG2, LlamaIndex, CrewAI, and custom agents. Built-in capabilities include distributed tracing, structured logging, Cloud Monitoring dashboards, bidirectional streaming, IAM-based agent identity, VPC Service Controls, CMEK encryption, and Private Service Connect for VPC isolation.

The Agents CLI accelerates deployment further with pre-built templates for ReAct, RAG, and multi-agent patterns, Terraform-based infrastructure automation, and Cloud Build CI/CD pipelines.

**Trogonai:** Self-hosted only. Production deployment requires running a NATS cluster and all service binaries. No managed runtime, no auto-scaling, no Agents CLI.

**Gap:** Trogonai production deployment requires significant platform engineering effort.

---

### 2.2 Model Garden — Unified Model API

Model Garden exposes over 100 models through a single parameter on `LlmAgent`. The model name is the only change required to switch between Gemini, Claude, Llama, Grok, Mistral, DeepSeek, Gemma, Qwen, Imagen, Veo, and Lyria. Authentication, SDK, and endpoint are identical across all models. The full model families available include Gemini Pro, Flash, and Flash-Lite; Anthropic Claude; Meta Llama; xAI Grok; Mistral AI; DeepSeek; Google Gemma; and Qwen.

**Trogonai:** One runner binary per model. Adding a new model requires implementing a new runner crate in Rust.

**Gap:** Trogonai requires significant engineering effort to add each new model.

---

### 2.3 Memory Bank (detailed API)

The Memory Bank sits across Build and Scale and its sub-APIs are worth enumerating in full.

`fetch-memories` retrieves all stored memories for a user scope. The `profiles` API provides aggregated per-user fact profiles that accumulate across all sessions and let agents personalize responses. The `revisions` API tracks how a memory evolved over time — each update creates a new revision. `ingest-events` accepts a stream of conversation events and generates memories from them asynchronously, decoupling memory creation from session completion.

The full memory lifecycle is: ingest events → generate memories (each memory is created, updated, or deleted) → fetch or search memories → inject into agent context.

**Trogonai:** No equivalent at any level.

---

### 2.4 Code Execution Sandbox

ADK provides two sandboxed code execution options. The built-in executor runs agent-generated Python code in a hermetic environment and returns the result inline. The GKE sandbox executor runs code in an isolated GKE container for stronger security boundaries. Both support data analysis, mathematical computation, and workflow automation use cases.

**Trogonai:** `trogon-wasm-runtime` provides WASM-based sandboxed execution with a virtual filesystem, NATS broker integration, and task limiting. `trogon-codex-runner` provides Codex-based execution. The isolation model differs (WASM vs. container) but the capability is functionally present.

**Parity:** Both platforms have sandboxed code execution. Mechanisms differ.

---

## SECTION 3: GOVERN

### 3.1 Safety

The platform provides three layers of safety, each operating at a different point in the request lifecycle.

Model-level filters are configured per agent using a harm category and a threshold. Four harm categories are configurable: dangerous content, hate speech, harassment, and sexually explicit content. Each has four threshold levels from block-none to block-low-and-above. Non-configurable filters are always active for CSAM and severe PII leakage.

The DLP API provides text inspection and de-identification. It can classify text by PII type, credential type, financial data, or custom infoTypes, and can de-identify it through redaction, masking, tokenization, or pseudonymization. Custom keyword blocklists are supported. DLP applies to both agent inputs (prompts) and outputs (responses).

Gemini-as-Filter uses a secondary Gemini Flash or Flash-Lite model to evaluate agent outputs against custom policies. It supports multimodal analysis across text, images, video, and audio, and can detect drift, hallucinations, brand misalignment, and arbitrary custom policy violations.

Content Credentials (C2PA) attaches cryptographic provenance metadata to AI-generated images.

The callback system described in section 1.5 (`after_model_callback`) is the primary mechanism for inline output interception at the framework level.

**Trogonai:** No model-level filter API, no DLP integration, no Gemini-as-Filter, and no callback system. `trogon-transcript` records outputs retroactively but does not intercept them.

**Gap:** No real-time safety layer exists. Harmful outputs reach users as-is.

---

### 3.2 Semantic Governance Policies

Semantic Governance Policies (SGP) enforce natural language constraints on agent and tool actions.

Each policy targets either an agent scope (all actions by a specific agent) or a tool scope (exactly one tool within an agent). Constraints are written in plain English with explicit, measurable limits — monetary amounts, time durations, and geographic allowlists. Subjective phrasing is not allowed.

Each evaluation produces one of three verdicts. `ALLOW` lets the action proceed. `DENY` blocks the action and returns a human-readable rationale. `ALLOW_IF_CONFIRMED` is a future capability that will pause execution for human confirmation.

The enforcement chain is: the Agent Gateway intercepts the call, the Nova PDP identifies the calling agent via JWT, and the Conseca service evaluates the natural language constraints against the current prompt, chat history, proposed tool invocations, tool manifest, and agent- and tool-specific constraints.

Policies are created via the `gcloud ai-platform semanticGovernancePolicy create` CLI command, passing the agent ID, MCP server, tool name, and a constraints string. They are also configurable through the Google Cloud Console. Testing sub-pages and IAM assignment for policy identities are available separately.

**Trogonai:** No semantic governance policies. Policy enforcement must be coded manually inside each actor. There is no natural language constraint system, no automatic interception, and no human-readable denial rationale.

**Gap:** Cross-agent policy enforcement requires custom code in Trogonai. The platform provides it declaratively in plain English.

---

### 3.3 Agent Registry

The Agent Registry is a central inventory for agents, tools, and endpoints across an organization.

Agents are registered automatically when deployed via Agent Runtime, Cloud Run, or GKE, or manually for custom deployments. Remote MCP servers can be registered to make their tools discoverable to any orchestrator in the organization. External API endpoints can be registered with centralized governance policies applied to all agent access to those endpoints.

Discovery works via keyword and prefix search across the organization's full catalog. Any team can find any registered agent or tool. The Auth Manager binds each discovered tool to its required credential scheme so that agents discovering tools at runtime automatically receive the correct credentials.

`trogon-registry` in Trogonai is a runtime presence service backed by NATS KV with a 15-second heartbeat and 30-second TTL. It answers whether an agent is currently online. It does not provide versioning, access control per agent, MCP server inventory, endpoint governance, or auth binding.

**Gap:** Trogonai has runtime presence tracking. The platform has organizational governance and a structured agent inventory.

---

### 3.4 Agent Gateway

Agent Gateway governs all agentic connectivity in two modes.

Client-to-Agent (ingress mode) secures communications from external clients — Cursor, Claude Code, Gemini CLI — to agents running on Google Cloud. It controls which clients can access which agents and enforces identity verification on every inbound request.

Agent-to-Anywhere (egress mode) secures all outbound agent communications — agent to agent, agent to MCP server, agent to external API. It enforces access policies before any call leaves the platform.

Five control layers stack on every connection: Identity-Aware Proxy validates agent identity and permissions; IAM policies restrict agents to specific tools based on SPIFFE IDs; Model Armor screens traffic for prompt injection and data leakage; Semantic Governance Policies enforce context-aware execution controls; and Service Extensions allow delegation to custom authorization engines.

Configuration is per-agent and per-client with tool-level granularity, organization-folder-project-level grant hierarchy, and an `INSPECT_ONLY` dry-run mode for auditing before enabling enforcement. Agent identity is established via mTLS and DPoP. Cloud Logging and Cloud Trace are integrated for monitoring all gateway traffic.

**Trogonai:** NATS subjects provide routing. There is no rate limiting, no IAP, no mTLS agent identity, no IAM-based access control, no semantic policy enforcement, and no gateway-level traffic monitoring.

**Gap:** Trogonai has message routing but not governed, authenticated, policy-enforced traffic management.

---

### 3.5 Model Armor

Model Armor is a content security screening layer integrated into Agent Gateway. It screens for prompt injection attempts, jailbreak attempts, sensitive information leakage, and harmful content generation.

It operates across four traffic types simultaneously. Client-to-Agent traffic using the ADK protocol is screened at the `reasoningEngines.streamQuery` level. Agent-to-Agent traffic using the A2A v1 protocol is screened on Send Message, Agent Card, and JSON-RPC calls. Agent-to-MCP traffic is screened on `tools/call` and `prompts/get` requests and responses. Agent-to-LLM traffic using the OpenAI API is screened on chat completions, embeddings, messages, and threads.

Configuration involves enabling the API, creating a template with safety filters and thresholds, setting enforcement mode on the Agent Gateway (either `INSPECT_ONLY` for auditing or `INSPECT_AND_BLOCK` for enforcement), and optionally enabling redaction via DLP de-identify templates. IAM roles are distinct for template administration and runtime invocation.

Security findings are viewable in the console and integrate with Security Command Center.

**Trogonai:** No Model Armor equivalent. No prompt injection detection and no content screening at any layer.

**Gap:** Trogonai has no defense-in-depth content security layer.

---

## SECTION 4: OPTIMIZE

### 4.1 Evaluation

The platform provides three evaluation modes that together cover development, pre-production, and production quality assessment.

**Offline evaluation** runs fixed test suites against a specific agent version. Test cases are defined in a structured format that specifies the user input, the expected final response, and the expected sequence of tool calls. The `adk eval` CLI command runs these suites and reports results per metric. Ten built-in metrics are available: `tool_trajectory_avg_score` measures whether the agent called the right tools in the right order; `response_match_score` computes ROUGE-1 similarity to the expected response; `final_response_match_v2` measures semantic equivalence; `hallucinations_v1` measures whether the response is grounded in provided context; `safety_v1` measures policy compliance; `multi_turn_task_success_v1` measures goal completion across multiple turns; `rubric_based_final_response_quality_v1` evaluates against a custom quality rubric; and three additional reference-free metrics cover helpfulness, task success, and exact match.

**Simulated evaluation** generates test scenarios automatically from the agent's instructions and tool definitions. It creates diverse user personas and multi-turn synthetic conversations, simulating realistic edge cases without manual authoring. Environment simulation intercepts tool calls during evaluation and injects mocked responses, HTTP errors, latency spikes, or custom data — testing agent resilience without making real external calls. Prompt optimization analyzes evaluation failures, identifies the specific instructions that caused them, and proposes targeted refinements to the system prompt automatically.

**Online evaluation** samples live production traffic continuously. It scores every sampled conversation using the same metrics as offline evaluation, in real time. Quality alerts can be configured on any metric — if a score drops below a threshold, the platform sends an alert. This closes the feedback loop between production and development.

**Trogonai:** `trogon-e2e` tests infrastructure behavior only — that messages are delivered and services respond correctly. No quality metrics, no trajectory scoring, no hallucination detection, no simulated evaluation, no online monitoring, and no quality alerts exist.

**Gap:** Trogonai cannot evaluate agent quality in any form. This is the most critical enterprise gap — no agent ships to production without a quality baseline.

---

### 4.2 Observability and Topology

The platform provides four observability capabilities for agents in production.

Distributed tracing uses OpenTelemetry and integrates with Cloud Trace. Every agent invocation produces spans covering event ingestion, routing, tool calls, model calls, and response delivery. Traces are accessible in the console and exportable via OTLP.

Topology visualization derives an interactive graph of agent-to-agent and agent-to-MCP relationships from aggregated trace data and Agent Registry entries. The graph can be zoomed, nodes dragged, and individual components clicked for detail panels. A table view with filterable and sortable columns is available alongside the graph. The use cases include understanding dependencies, supporting governance audits, troubleshooting communication failures, and monitoring multi-agent interactions at scale.

Structured logging integrates with Cloud Logging per agent, per session, and per tool. Cloud Monitoring provides dashboards with custom metrics per agent, per model, and per tool.

**Trogonai:** `acp-telemetry` provides OTel setup with OTLP export, vendor-neutral and compatible with Grafana, Jaeger, and Datadog. Trace context propagates through NATS message headers across the full pipeline. Infrastructure observability is strong. Topology visualization, agent-quality dashboards, and agent observability platform integrations (AgentOps, Arize, Galileo, Phoenix, LangWatch) are missing.

**Partial parity:** OTel traces are well-implemented. Topology and agent-quality analytics are not present.

---

### 4.3 Example Store

The Example Store is a managed repository for few-shot examples that improve agent quality at inference time without requiring redeployment.

Up to 50 `ExampleStore` resources can exist per project per region. Examples come in two types: correct examples that demonstrate the expected behavior in a given scenario, and corrective examples that show the right response when the agent previously made a mistake. Once examples are uploaded, ADK agents linked to an Example Store auto-retrieve relevant examples at inference time using cosine similarity search, optionally filtered by function name. For non-ADK integrations, retrieval can be triggered manually. Quality improvements take effect immediately in production without any code changes or redeployment.

**Trogonai:** No Example Store. No few-shot example management. Any improvement to agent prompting requires code changes and redeployment.

**Gap:** Trogonai has no mechanism for iterative quality improvement without code changes.

---

## Summary Tables

### BUILD layer

| Capability | Platform API | Trogonai |
|---|---|---|
| Agent definition | `LlmAgent` — declarative, 4 languages, model/tools/schema/planner | Rust trait only |
| SequentialAgent | Sub-agents in fixed order, state flows via output keys | Not present |
| ParallelAgent | Sub-agents run concurrently, independent state | Not present |
| LoopAgent | Repeats sub-agents up to N iterations, early-exit flag | Not present |
| Inline function tool | Annotated function → auto-schema | MCP server required |
| AgentTool | Wrap any agent as a callable tool | Not present |
| LongRunningFunctionTool | Streaming / waiting tool with intermediate results | Not present (vault approvals only) |
| OpenAPI tool generation | Auto-generates from spec | Not present |
| MCP tools | `McpToolset` — local subprocess or remote HTTP | `trogon-mcp` — **parity** |
| Callbacks (6 hooks) | `before/after_agent/model/tool_callback` | Not present |
| Session SDK | create / get / list / delete — 3 backends | ACP messages only |
| Session REST API | POST / GET / DELETE sessions + events | Not present |
| Memory `search_memory` | `VertexAiMemoryBankService` + `PreloadMemoryTool` | Not present |
| `GenerateMemories` REST | Long-running operation, CREATED/UPDATED/DELETED | Not present |
| Context compaction | Built-in | `trogon-compactor` — **parity** |
| Artifact management | save / load / list — versioned, GCS-backed | Not present |
| Runner + event stream | `run_async` — typed event stream | NATS pub/sub only |
| Local dev server | `adk web` — browser UI + trace viewer | Not present |
| ToolContext APIs | search_memory, request_credential, list_artifacts | Not present |
| A2A protocol | Open standard — expose + consume — cross-framework | Not present |
| RAG Engine | Managed 6-step pipeline, 7 vector DB backends, multimodal | Not present |
| Vector Search 2.0 | Collections, auto-tuning, hybrid search | Not present |
| Google Search grounding | Built-in real-time retrieval tool | Not present |
| Agent Studio | Visual no-code agent design UI | Not present |
| Agent Garden | Pre-built templates (RAG, ReAct, multi-agent) | Not present |
| 3-legged OAuth | Inline consent flow in tool context | Not present |

### SCALE layer

| Capability | Platform API | Trogonai |
|---|---|---|
| Managed auto-scaling runtime | Deploy via CLI → REST query API | Self-hosted NATS + binaries |
| Multi-framework runtime | ADK, LangChain, LangGraph, AG2, LlamaIndex, CrewAI | Single framework (Rust/ACP) |
| Unified model API (100+ models) | One model name parameter | One runner binary per model |
| Memory Bank + Profiles + Revisions | GenerateMemories, FetchMemories, IngestEvents | Not present |
| Code execution sandbox | Built-in executor or GKE sandbox | `trogon-wasm-runtime` — **parity** |
| Bidirectional streaming | Built-in | Not present |

### GOVERN layer

| Capability | Platform API | Trogonai |
|---|---|---|
| Model safety filters | Per-category harm thresholds | Not present |
| DLP / PII redaction | Redact, mask, tokenize, custom infoTypes | Not present |
| Gemini-as-Filter | Secondary model policy evaluation, multimodal | Not present |
| Semantic Governance Policies | Natural language constraints, ALLOW/DENY/ALLOW_IF_CONFIRMED | Not present |
| Agent Registry | Agents + MCP servers + endpoints; discovery; auth binding | Runtime presence only |
| Agent Gateway (ingress + egress) | IAP + IAM + Model Armor + SGP + mTLS | NATS routing only |
| Model Armor | Screens ADK / A2A / MCP / OpenAI traffic | Not present |
| Security findings | Console + Security Command Center integration | Not present |
| Credential isolation (token proxy) | Not present | `trogon-secret-proxy` — **Trogonai ahead** |

### OPTIMIZE layer

| Capability | Platform API | Trogonai |
|---|---|---|
| Offline evaluation + 10 metrics | evalset format + `adk eval` CLI | Not present |
| Simulated evaluation + env simulation | AI personas; mock tool responses, errors, latency | Not present |
| Prompt optimization | Auto-generate refined instructions from failure analysis | Not present |
| Online evaluation + quality alerts | Continuous production sampling + threshold alerts | Not present |
| Example Store | Upload examples; auto-retrieve at inference; no redeployment | Not present |
| Distributed tracing | OTel + Cloud Trace | `acp-telemetry` (OTel) — **parity** |
| Topology visualization | Agent-to-agent + agent-to-MCP graph; interactive | Not present |
| Structured logging + monitoring | Cloud Logging + Cloud Monitoring per agent | OTel logs only |

### Where Trogonai Is Ahead

| Capability | Trogonai | Platform |
|---|---|---|
| Webhook event ingestion | 11 sources, HMAC-validated, JetStream delivery | Not present |
| Credential isolation (token proxy) | VGS-style proxy, AES-256-GCM, Argon2id KDF, 90-day audit log | IAM / env vars — agent reads credentials directly |
| Distributed job scheduling | Interval/scheduled jobs, event-sourced registry, leader election | Not present (external Cloud Scheduler) |
| Transport flexibility | NATS (JetStream + Core), WebSocket, stdio | HTTP/gRPC only |
| Feature flags | Split.io via `trogon-splitio` | Not present |

---

## Conclusion

This analysis is based on direct exploration of https://docs.cloud.google.com/gemini-enterprise-agent-platform/build/adk and all reachable sub-pages across the four platform sections (Build, Scale, Govern, Optimize).

Trogonai is a strong infrastructure platform. Webhook event ingestion from 11 sources, credential isolation via a token proxy, distributed job scheduling, NATS transport, and feature flags are capabilities the Gemini Enterprise Agent Platform does not provide and deliberately delegates to the broader GCP ecosystem. In these areas Trogonai is genuinely ahead.

The gap is in the agent authoring, runtime, governance, and quality layers. In priority order to close it:

1. **Evaluation framework** — no agent ships to production without a quality baseline; hallucination detection and trajectory scoring are minimum enterprise requirements
2. **REST control plane** — non-Rust developers need HTTP access to sessions and agent invocation
3. **Callback / middleware system** — before/after model and before/after tool hooks on the actor lifecycle; unblocks safety, PII redaction, and governance enforcement
4. **Workflow agent primitives** — SequentialActor, ParallelActor, LoopActor as NATS-native patterns
5. **Semantic memory** — search_memory API backed by a vector store; cross-session knowledge retrieval
6. **Unified model API** — single dispatch interface instead of one runner binary per model
7. **A2A protocol adoption** — cross-framework agent interoperability
8. **Topology observability** — agent-to-agent call graph derived from existing OTel traces
