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

For each section, this document shows exactly what the platform API provides and how Trogonai compares.

---

## SECTION 1: BUILD

### 1.1 Agent Definition — `LlmAgent`

The core authoring primitive. A developer declares an agent in a single constructor call across four languages:

```python
# Python
agent = LlmAgent(
    model="gemini-flash-latest",        # or "claude-3-5-sonnet", "llama-3.1-70b", etc.
    name="triage_agent",
    description="Triages support tickets",
    instruction="Classify the ticket and assign priority.",
    tools=[get_ticket_info, escalate_ticket],
    input_schema=TicketInput,           # Pydantic model — validates input structure
    output_schema=TicketClassification, # enforces structured JSON output
    output_key="classification",        # saves result to session state under this key
    include_contents="default",         # "none" to disable conversation history
    generate_content_config=GenerateContentConfig(temperature=0.2),
    planner=BuiltInPlanner(),           # or PlanReActPlanner() for plan-then-act
    code_executor=BuiltInCodeExecutor()
)
```

Same API in TypeScript (Zod schemas), Go (`llmagent.New(llmagent.Config{...})`), and Java (`LlmAgent.builder()...build()`).

**Trogonai:** No `LlmAgent`. An agent is a Rust binary implementing the ACP `Agent` trait or the `EntityActor` pattern. No declarative definition, no multi-language support, no `model` parameter, no `output_schema`, no `planner`.

**Gap:** Authoring a Trogonai agent requires Rust expertise. The platform reduces it to a single constructor call in four languages.

---

### 1.2 Workflow Agents — Sequential, Parallel, Loop

Three deterministic composition primitives — no LLM involved in control flow:

**SequentialAgent** — sub-agents run one after another, sharing `InvocationContext`. State flows via `output_key` + `{template}` interpolation:

```python
code_writer   = LlmAgent(instruction="Write the code.",           output_key="generated_code")
code_reviewer = LlmAgent(instruction="Review: {generated_code}", output_key="review_comments")
code_refactor = LlmAgent(instruction="Refactor using: {review_comments}")

pipeline = SequentialAgent(name="CodePipeline", sub_agents=[code_writer, code_reviewer, code_refactor])
```

**ParallelAgent** — sub-agents run concurrently; no automatic state sharing between branches:

```python
fetcher = ParallelAgent(
    name="DataFetcher",
    sub_agents=[fetch_crm, fetch_jira, fetch_slack]
    # each stores result via output_key; downstream SequentialAgent merges
)
```

**LoopAgent** — repeats until `max_iterations` or `escalate=True`:

```python
refiner = LoopAgent(name="RefinementLoop", sub_agents=[generator, reviewer], max_iterations=5)

# Sub-agent signals stop early:
def exit_loop(tool_context: ToolContext):
    tool_context.actions.escalate = True
    return {"status": "done"}
```

All constructors identical across Python, TypeScript, Go, Java.

**Trogonai:** No `SequentialAgent`, `ParallelAgent`, or `LoopAgent`. Sequential steps must be hard-coded in an actor or chained via NATS messages. Parallel execution requires publishing to multiple subjects and aggregating replies manually. `trogon-automations` handles trigger-based flows but is not a general pipeline primitive.

**Gap:** Deterministic pipeline composition does not exist in Trogonai as a developer API.

---

### 1.3 Custom Agents — `BaseAgent`

```python
class MyCustomAgent(BaseAgent):
    async def _run_async_impl(self, ctx: InvocationContext) -> AsyncGenerator[Event, None]:
        result = await my_external_service.call(ctx.user_content)
        yield Event(author=self.name, content=types.Content(parts=[types.Part(text=result)]))
```

`BaseAgent` provides: `sub_agents` list, `find_agent(name)`, `parent_agent` (auto-set), all lifecycle callbacks.

**Trogonai:** `EntityActor` in `trogon-actor` is functionally equivalent — a custom handler processing NATS messages with persistent state. Rust-only; no event generator pattern.

---

### 1.4 Tool Definition

**Function tools** — write a function, ADK auto-generates the LLM schema from type hints and docstring:

```python
def escalate_ticket(ticket_id: str, reason: str) -> dict:
    """Escalates a support ticket to tier-2.
    Args:
        ticket_id: The ticket identifier.
        reason: Why it is being escalated.
    """
    return {"status": "escalated", "assigned_to": "tier2"}

agent = LlmAgent(tools=[escalate_ticket])
```

Same in TypeScript (`FunctionTool` + Zod), Go (`functiontool.New()`), Java (`FunctionTool.create()`).

**LongRunningFunctionTool** — for operations that yield intermediate results or require waiting:
```python
approval_tool = LongRunningFunctionTool(func=request_human_approval)
```

**AgentTool** — wrap any agent as a callable tool; parent LLM calls it like a function:
```python
tool = agent_tool.AgentTool(agent=summarizer_agent)
orchestrator = LlmAgent(tools=[tool])
```

**OpenAPI tools** — auto-generate tools from any OpenAPI/Swagger spec; zero manual implementation.

**MCP tools** — connect to any MCP server (local subprocess or remote HTTP):
```python
# Local subprocess
McpToolset(
    connection_params=StdioConnectionParams(
        server_params=StdioServerParameters(command="npx", args=["-y", "@mcp/filesystem"])
    ),
    tool_filter=["read_file", "list_directory"]
)

# Remote HTTP MCP server
McpToolset(
    connection_params=StreamableHTTPConnectionParams(
        url="https://mapstools.googleapis.com/mcp",
        headers={"X-Goog-Api-Key": "YOUR_KEY"}
    )
)
```

`McpToolset` handles: connection lifecycle, `list_tools` discovery, schema adaptation to ADK format, `call_tool` dispatch.

**Trogonai:** Tools via MCP only (`trogon-mcp`). No inline function tool, no `AgentTool`, no `LongRunningFunctionTool`, no OpenAPI generation. Human-in-the-loop only in `trogon-vault-approvals` for credential writes.

**Gap:** Every Trogonai tool requires a running MCP server. ADK supports 5-line inline definition and OpenAPI auto-generation.

---

### 1.5 Callbacks — Runtime Interception Hooks

Six hooks that fire at precise execution points. Returning a value short-circuits the default behavior:

| Callback | Fires when | Can do |
|---|---|---|
| `before_agent_callback(ctx)` | Before agent starts | Return `Content` to skip agent |
| `after_agent_callback(ctx)` | After agent finishes | Return `Content` to replace output |
| `before_model_callback(ctx, request)` | Before LLM call | Return `LlmResponse` to skip/modify |
| `after_model_callback(ctx, response)` | After LLM responds | Return `LlmResponse` to sanitize |
| `before_tool_callback(ctx, tool_input)` | Before tool executes | Return `dict` to skip tool |
| `after_tool_callback(ctx, tool_output)` | After tool completes | Return `dict` to replace result |

```python
def sanitize_output(callback_context: CallbackContext, llm_response: LlmResponse):
    if contains_pii(llm_response.content):
        return LlmResponse(content=redact_pii(llm_response.content))
    return None  # allow through

def enforce_table_policy(tool_context: ToolContext, tool_input: dict) -> dict | None:
    policy = tool_context.session.state.get("access_policy", {})
    if tool_input.get("table") not in policy.get("allowed_tables", []):
        return {"error": "Access denied to this table"}
    return None

agent = LlmAgent(after_model_callback=sanitize_output, before_tool_callback=enforce_table_policy)
```

**Trogonai:** No callback system. No interception points. Safety, PII redaction, and policy enforcement must be reimplemented manually inside every actor.

**Gap:** Callbacks are the primary mechanism for safety enforcement and governance. Their absence makes cross-cutting concerns expensive to implement consistently.

---

### 1.6 Context APIs

Typed context hierarchy controlling what agents and tools can access at runtime:

| Context | Available to | Key API |
|---|---|---|
| `ReadonlyContext` | Read-only callbacks | `invocation_id`, `agent_name`, `state` (read-only) |
| `CallbackContext` | Lifecycle callbacks | `state` (read/write), `load_artifact(filename, version)`, `save_artifact(filename, part)`, `user_content` |
| `ToolContext` | Tool functions | Everything above + `request_credential(auth_config)`, `get_auth_response(auth_config)`, `list_artifacts()`, `search_memory(query)`, `function_call_id`, `actions` |
| `InvocationContext` | Agent core | Full session, all services, `end_invocation` flag |

`request_credential(auth_config)` initiates OAuth or API key flow inside a tool. `get_auth_response()` retrieves the credential after user consent.

**Trogonai:** `ActorContext` gives state access (NATS KV) and `spawn_agent()`. MCP tools are pure JSON-in/JSON-out. No `ToolContext`, no artifact access in tools, no in-tool memory search, no credential request flow.

**Gap:** Tools in Trogonai have no access to session context, memory, or artifacts.

---

### 1.7 Session Management

**ADK SDK interface:**

```python
# Create
session = await session_service.create_session(
    app_name="support_app", user_id="user_42", state={"tier": "premium"}
)
# Read
session = await session_service.get_session(app_name=..., user_id=..., session_id=session.id)
sessions = await session_service.list_sessions(app_name=..., user_id=...)
# Update
await session_service.append_event(session, event)
# Delete
await session_service.delete_session(app_name=..., user_id=..., session_id=session.id)
```

**Direct REST API** (no ADK required):

```http
# List sessions
GET https://{LOCATION}-aiplatform.googleapis.com/v1beta1/projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/{AGENT_ENGINE_ID}/sessions
?filter=user_id="USER_ID"

# Create session
POST .../sessions
Body: { "userId": "USER_ID", "ttl": "86400s", "expire_time": "2026-12-31T00:00:00Z" }

# Get session
GET .../sessions/{SESSION_ID}

# Delete session
DELETE .../sessions/{SESSION_ID}

# List events
GET .../sessions/{SESSION_ID}/events

# Append event
POST .../sessions/{SESSION_ID}/events
Body: {
  "author": "user", "invocationId": "1", "timestamp": "...",
  "config": { "content": { "role": "user", "parts": [{"text": "..."}] } }
}
```

Session TTL default: 365 days. Events support both structured content and arbitrary `rawEvent` payloads.

Three SDK implementations: `InMemorySessionService`, `DatabaseSessionService` (async SQLite/Postgres), `VertexAiSessionService` (fully managed).

**Trogonai:** ACP protocol messages (new_session, fork_session, load_session, close_session) processed by the runner internally. No `SessionService` interface. No REST API for sessions. Session state = actor state in NATS KV.

**Gap:** Sessions are a protocol concept in Trogonai, not an API. External callers cannot manage sessions without speaking raw NATS/ACP.

---

### 1.8 Memory — Memory Bank

**ADK SDK interface:**
```python
await memory_service.add_session_to_memory(session)
results = await memory_service.search_memory("user's preferred escalation path")

# Inside a tool
snippets = tool_context.search_memory(query)
```

Built-in tools: `PreloadMemoryTool` (auto-retrieve at turn start), `LoadMemoryTool` (agent-triggered).

**REST API — `GenerateMemories`:**

```python
# Long-running operation
response = client.agent_engines.memories.generate(
    agent_engine=AGENT_ENGINE_RESOURCE_NAME,
    # Data source — choose one:
    vertex_session_source={"session": "SESSION_RESOURCE_NAME"},
    # or:
    direct_contents_source={"events": [...]},
    # or:
    direct_memories_source={"memories": [...]},  # up to 5 pre-extracted facts
    # Scope — determines which existing memories are eligible for consolidation:
    scope={"user_id": "123"},
    # Options:
    allowed_topics=["billing", "product_issues"],
    metadata={"source": "support_chat"},
    metadata_merge_strategy="MERGE",  # or "OVERWRITE" or "REQUIRE_EXACT_MATCH"
    disable_consolidation=False,
    wait_for_completion=True
)
# Response: GenerateMemoriesResponse with list of memories
# Each memory has action: CREATED | UPDATED | DELETED
```

Additional Memory Bank sub-APIs:
- `fetch-memories`: retrieve all memories for a user scope
- `profiles`: user memory profiles aggregating facts across sessions
- `revisions`: memory revision history
- `ingest-events`: stream conversation events for automatic memory generation

Implementations: `InMemoryMemoryService` (keyword matching, prototyping), `VertexAiMemoryBankService` (semantic vector search, production).

**Trogonai:** `trogon-transcript` (append-only log, no search). `trogon-compactor` (in-context compaction, not cross-session). No `GenerateMemories`, no `search_memory()`, no `PreloadMemoryTool`, no vector store.

**Gap:** Trogonai agents have no long-term memory. Every session starts from zero.

---

### 1.9 Artifact Management

Versioned binary objects (PDFs, images, files) scoped to session or user:

```python
# Save — returns version number
version = await context.save_artifact("report.pdf",
    types.Part.from_bytes(pdf_bytes, "application/pdf"))

# Load — latest by default, or specific version
report     = await context.load_artifact("report.pdf")
report_v2  = await context.load_artifact("report.pdf", version=2)

# List
filenames = await context.list_artifacts()
versions  = await artifact_service.list_versions("report.pdf")

# User-scoped — accessible across all sessions for this user
await context.save_artifact("user:preferences.json", prefs_part)
```

Implementations: `InMemoryArtifactService` (ephemeral, testing), `GcsArtifactService` (Google Cloud Storage, production).

**Trogonai:** No artifact system. Binary data management is not a platform capability.

**Gap:** No equivalent in Trogonai.

---

### 1.10 Running an Agent — The Runner

```python
runner = Runner(
    agent=root_agent,
    app_name="support_app",
    session_service=InMemorySessionService(),
    artifact_service=GcsArtifactService(bucket="my-bucket"),
    memory_service=VertexAiMemoryBankService(...)
)

async for event in runner.run_async(
    user_id="user_42", session_id="session_1",
    new_message=types.Content(role="user", parts=[types.Part(text="My payment failed")])
):
    if event.is_final_response():
        print(event.content.parts[0].text)
    # event types: tool_call, tool_result, state_delta, agent_transfer, model_response
```

Local development (zero configuration required):
```bash
adk web agents/         # browser UI + trace viewer (event→request→response→graph)
adk run agents/ --input "classify this ticket"
adk eval agents/agent.py evals/test.evalset.json
```

**Trogonai:** No `Runner`. Execution is reactive NATS pub/sub. No event stream. No local dev server.

**Gap:** Running a Trogonai agent requires a NATS cluster and subject routing knowledge.

---

### 1.11 Agent2Agent (A2A) Protocol

Open standard at https://a2a-protocol.org for cross-framework agent interoperability. An ADK agent, LangGraph agent, CrewAI agent, or custom agent can all call each other regardless of framework.

**Expose** — make your agent callable:
```python
# Agent serves an AgentCard at /.well-known/agent.json describing its capabilities
# Other agents call via HTTP POST /tasks/send
```

**Consume** — call another agent:
```python
a2a_agent = A2AAgent(agent_url="https://other-service.example.com/agent")
orchestrator = LlmAgent(sub_agents=[a2a_agent])
```

Protocol methods: `tasks/send` (send a task), `tasks/get` (poll result), streaming responses, `AgentCard` capability description.

Available in Python, Go, Java. Deployable on Agent Runtime (`create-an-a2a-agent`), usable from Agent Runtime (`use-an-a2a-agent`). Model Armor screens A2A traffic on Agent Gateway.

**Trogonai:** No A2A protocol. Inter-agent communication uses NATS subjects — functional within the Trogonai ecosystem but not interoperable with external agent frameworks.

**Gap:** Trogonai is a closed ecosystem for agent communication.

---

### 1.12 RAG Engine

Managed 6-step RAG pipeline:

1. **Ingest**: local files, Cloud Storage, Google Drive
2. **Transform**: document parsing, chunking (semantic / fixed-size / hierarchical), layout parsing, LLM-based parsing
3. **Embed**: Vertex AI embeddings, Gemini embeddings, or custom embedding model
4. **Index**: RAG corpus (like a table of contents for the knowledge base)
5. **Retrieve**: vector similarity search + hybrid search (dense + sparse)
6. **Generate**: retrieved context injected into LLM prompt, with source attribution

Vector DB choices: RAG Managed DB, Vertex AI Vector Search, Feature Store, Weaviate, Pinecone, Vertex AI Search.

Deployment modes: Spanner-backed (high throughput), Serverless (zero config). Multimodal RAG supported (text, images in live sessions). CMEK encryption supported. 21+ regions.

```bash
# Console UI
console.cloud.google.com/agent-platform/rag

# Python SDK quickstart available; v1 and v1beta1 API
```

**Google Search grounding** — real-time web retrieval as a built-in tool:
```python
agent = LlmAgent(
    tools=[google_search_tool],
    generate_content_config=GenerateContentConfig(search_grounding=True)
)
```

**Agent Garden** provides a pre-built RAG agent template (deployable from GitHub, includes debug panel showing retrieval and reasoning).

**Trogonai:** No RAG pipeline, no document ingestion, no vector search, no hybrid retrieval, no grounding, no source attribution.

**Gap:** Knowledge-intensive agents cannot be built on Trogonai without implementing a complete RAG pipeline externally.

---

### 1.13 Vector Search 2.0

A new collections-based architecture replacing the original index-as-a-service model:

| Concept | Description |
|---|---|
| **Collection** | Container of related JSON objects (like a database table); has a schema with strict or relaxed validation |
| **Data Object** | Individual JSON object stored in a Collection; fundamental storage unit |
| **Collection Index** | Enables ANN search across a Collection; multiple indexes per Collection (one per vector field) |

Key capabilities vs Vector Search 1.0:
- Auto-tuning: eliminates manual VM/replica configuration
- Built-in embedding: auto-populate vector fields using integrated models
- BYOE (Bring Your Own Embeddings)
- Rich query: vector similarity + payload filtering in one request
- Unified storage: retrieval, filtering, and document management in one system
- Two pricing models: usage-based and resource-based

Sub-pages: `collections`, `data-objects`, `query-search/query`, `query-search/search`, `indexes`.

**Trogonai:** No vector search. No collections-based storage. No ANN index.

**Gap:** Semantic retrieval does not exist in Trogonai at any level.

---

### 1.14 Authentication

Three mechanisms for agents calling external services:
- **API keys** — static; injected via `ToolContext.request_credential()`
- **2-legged OAuth** (service-to-service) — client credentials flow; agent authenticates as itself
- **3-legged OAuth** (user-delegated) — authorization code flow; `request_credential()` initiates consent; `get_auth_response()` retrieves the token

Auth Manager in Agent Registry binds discovered tools to their required credential schemes. Agents dynamically discovering tools via the registry automatically get the correct auth binding.

IAM docs: `auth-with-3lo`, `auth-with-2lo`, `auth-with-api-key`.

**Trogonai:** Token proxy (`trogon-secret-proxy`) isolates static API keys — agent holds opaque token, proxy resolves to real key. Stronger isolation than IAM env vars for static keys. No 3-legged OAuth flow. No dynamic auth binding for discovered tools.

**Partial parity / Gap:** Trogonai excels at static key isolation. 3-legged OAuth and dynamic auth binding for tool discovery do not exist.

---

### 1.15 Agent Studio and Agent Garden

**Agent Studio** — visual no-code/low-code agent design UI at `agent-studio/design-agents`. Non-developers compose agents, configure tools, set instructions, and test interactively. Deployable directly to Agent Runtime.

**Agent Garden** — curated library of pre-built agent templates with GitHub source access. Templates: ReAct, RAG, multi-agent. Each provides immediate functionality, interactive chat test interface, debug panel, and full customization via GitHub.

**Trogonai:** No visual design UI, no pre-built templates. Every agent is built from scratch in Rust.

**Gap:** Non-developer access to the platform does not exist in Trogonai.

---

## SECTION 2: SCALE

### 2.1 Agent Runtime (Reasoning Engine)

Fully managed, auto-scaling runtime based on the `ReasoningEngine` API resource:

```bash
adk deploy agent_engine \
  --project=$PROJECT_ID --region=$LOCATION_ID \
  --display_name="Triage Agent" \
  ./agents/triage_agent/
```

**Query deployed agent:**
```bash
POST https://{LOCATION}-aiplatform.googleapis.com/v1/projects/{PROJECT}/locations/{LOCATION}/reasoningEngines/{ID}:query

# Python SDK
engine = vertexai.agent_engines.get("projects/.../reasoningEngines/RESOURCE_ID")
response = engine.query(input={"message": "..."})
```

**Manage deployed agents** (sub-pages):
- `deploy-an-agent`, `manage-deployed-agents`, `manage-agent-access`
- `tracing`, `logging`, `monitoring` — built-in observability
- `optimize-and-scale`, `private-service-connect-interface`
- `bidirectional-streaming` — streaming responses
- `agent-identity` — IAM-based agent identity

**Agents CLI** — accelerated deployment with pre-built templates (ReAct, RAG, multi-agent), Terraform infrastructure automation, Cloud Build CI/CD pipelines.

**Supported frameworks:** ADK, LangChain, LangGraph (full), AG2, LlamaIndex (SDK), CrewAI, custom agents.

**VPC-SC + CMEK** — compliance and encryption support.

**Trogonai:** Self-hosted only. Requires NATS cluster, all service binaries, external dependencies. No managed runtime, no auto-scaling, no Agents CLI.

**Gap:** Trogonai production deployment requires platform engineering expertise.

---

### 2.2 Model Garden — Unified Model API

100+ models, one parameter:

```python
LlmAgent(model="gemini-flash-latest")     # Google Gemini
LlmAgent(model="claude-3-5-sonnet@...")   # Anthropic Claude
LlmAgent(model="llama-3.1-70b-instruct") # Meta Llama
LlmAgent(model="grok-3")                 # xAI Grok
LlmAgent(model="mistral-large@...")      # Mistral AI
```

Families: Gemini (Pro/Flash/Flash-Lite), Imagen, Veo, Lyria, Claude, Grok, Llama, Mistral, DeepSeek, Gemma, Qwen. Same auth, same SDK, same endpoint for all.

**Trogonai:** One runner binary per model. Adding a model = implementing a new runner crate.

**Gap:** Trogonai requires significant engineering to add each new model.

---

### 2.3 Memory Bank (detailed API)

See §1.8 for the `GenerateMemories` REST API. Additional sub-API details:

- **`fetch-memories`**: retrieve all stored memories for a user scope
- **`profiles`**: user memory profiles — aggregated facts about a user across all sessions; agents use profiles to personalize responses
- **`revisions`**: memory revision history — track how a memory evolved over time
- **`ingest-events`**: stream conversation events; platform extracts memories asynchronously

Memory lifecycle: `IngestEvents` → `GenerateMemories` (CREATED/UPDATED/DELETED) → `FetchMemories` / `SearchMemory` → agent context injection.

**Trogonai:** No equivalent at any level.

---

### 2.4 Code Execution Sandbox

```python
agent = LlmAgent(code_executor=BuiltInCodeExecutor())
# or for GKE-isolated sandbox:
agent = LlmAgent(code_executor=GkeSandboxCodeExecutor())
```

Hermetic sandbox: agents generate and run Python code, returning results inline. Use cases: data analysis, mathematical computation, workflow automation. Troubleshooting guide: `troubleshooting/code-execution`.

**Trogonai:** `trogon-wasm-runtime` provides WASM-based sandboxed execution with virtual filesystem, NATS broker, task limiting. `trogon-codex-runner` provides Codex-based execution. Functional parity at isolation level.

**Parity:** Both have sandboxed code execution. Mechanisms differ (WASM vs. container).

---

## SECTION 3: GOVERN

### 3.1 Safety

**Model-level filters** — four configurable harm categories:
```python
generate_content_config=types.GenerateContentConfig(
    safety_settings=[
        types.SafetySetting(
            category=types.HarmCategory.HARM_CATEGORY_DANGEROUS_CONTENT,
            threshold=types.HarmBlockThreshold.BLOCK_LOW_AND_ABOVE,
        ),
        types.SafetySetting(
            category=types.HarmCategory.HARM_CATEGORY_HATE_SPEECH,
            threshold=types.HarmBlockThreshold.BLOCK_MEDIUM_AND_ABOVE,
        ),
    ]
)
```

Non-configurable filters always active: CSAM, severe PII leakage.

**DLP API** — text inspection and de-identification:
- Classify: PII, credentials, financial data, custom infoTypes
- De-identify: redaction, masking, tokenization, pseudonymization
- Custom keyword blocklists
- Protects both agent inputs (prompts) and outputs (responses)

**Gemini-as-Filter** — use Gemini Flash/Lite as a secondary evaluation model:
- Multimodal analysis: text, images, video, audio
- Detects: drift, hallucinations, brand misalignment, custom policy violations

**Content Credentials (C2PA)** — cryptographic provenance metadata on AI-generated images.

**Callback guardrails** — `before/after_model_callback` for inline output interception (see §1.5).

**Trogonai:** No model-level filter API, no DLP integration, no Gemini-as-Filter, no callback system. `trogon-transcript` records outputs retroactively but does not intercept them.

**Gap:** No real-time safety layer. Harmful outputs reach users as-is.

---

### 3.2 Semantic Governance Policies

Natural Language Constraints (NLC) enforced at the tool and agent level:

**Three verdict types:**
- `ALLOW` — action proceeds
- `DENY` — action blocked with human-readable rationale
- `ALLOW_IF_CONFIRMED` — future capability: pause for human confirmation

**Rule scopes:**
- **Agent scope**: constraints apply to all actions by a specific agent
- **Tool scope**: constraints apply to exactly one tool within an agent

**Enforcement chain:**
1. Agent Gateway intercepts the tool/model call
2. Nova PDP identifies the calling agent via JWT
3. Conseca service evaluates NLC constraints against: current user prompt, chat history, suggested tool invocations, tool manifest, and agent/tool-specific constraints
4. Verdict + rationale returned

**CLI:**
```bash
gcloud ai-platform semanticGovernancePolicy create [policy-name] \
  --agent=[agent-id] \
  --mcp-server=[server] \
  --tool-name=[tool] \
  --natural-language-constraints="Only allow financial transactions under $500. Block any international transfers without manager approval. Restrict data access to the user's own records only."
```

Also configurable via Google Cloud Console. Rules must use plain English with explicit limits (monetary: "$500", temporal: "72 hours", geographic allowlists). No subjective phrasing allowed ("reasonable", "expensive").

Test sub-page: `govern/policies/test-policies`.
IAM assignment: `govern/policies/assign-identity-iam`.

**Trogonai:** No semantic governance policies. Policy enforcement must be coded manually inside each actor. No NLC, no automatic interception, no human-readable rationale on denial.

**Gap:** Cross-agent policy enforcement requires custom code in Trogonai. The platform provides it declaratively in plain English.

---

### 3.3 Agent Registry

Central hub for agent and tool inventory:

- **Agent registration**: automatic from Agent Runtime, Cloud Run, GKE; or manual for custom deployments
- **MCP server registration**: register remote MCP servers and make their tools discoverable to orchestrators platform-wide
- **Endpoint registration**: register external APIs with centralized governance policies applied to all agent access
- **Discovery**: keyword and prefix search across the organization's catalog; agents and tools are findable by any team
- **Auth Manager**: bind discovered tools to their required credential schemes (API key, 2LO, 3LO); agents dynamically authenticating to discovered tools get the right credentials automatically
- **ADK integration**: resolve registered endpoints dynamically in orchestrator agents (`govern/agent-registry`)
- **Share agents**: `govern/share-agent` — grant access to agents across teams or projects

**Trogonai:** `trogon-registry` is a runtime discovery service (NATS KV, 15s heartbeat, 30s TTL). Answers "is this agent online?" — not "what agents exist organization-wide, what version, who can call them." No MCP server registry, no endpoint governance, no auth binding.

**Gap:** Trogonai has runtime presence tracking; the platform has organizational governance and inventory.

---

### 3.4 Agent Gateway

Networking component governing all agentic connectivity:

**Two modes:**
- **Client-to-Agent (Ingress)**: secures communications from external clients (Cursor, Claude Code, Gemini CLI) to agents on Google Cloud; controls which clients can access which agents
- **Agent-to-Anywhere (Egress)**: secures agent-to-agent, agent-to-MCP, agent-to-API communications; enforces access policies before calls leave the platform

**Access control layers:**
1. **Identity-Aware Proxy (IAP)** — validates agent identity and permissions
2. **IAM Policies** — restricts agents to specific tools based on SPIFFE ID
3. **Model Armor** — runtime protection against prompt injection and data leakage (see §3.5)
4. **Semantic Governance Policies** — context-aware execution controls (see §3.2)
5. **Service Extensions** — delegate to custom authorization engines

**Configuration controls:**
- Per-agent and per-client access rules
- Tool-level granularity (read-only vs. read-write)
- Organization/folder/project-level grants
- MCP server access (registered or unregistered)
- `INSPECT_ONLY` dry-run audit mode before enabling enforcement

**Authentication:** mTLS + DPoP for agent identity.

**Deployment:** `scale/runtime/agent-gateway-runtime-deploy`.
**Monitoring:** `govern/gateways/monitor-agent-gateway` — Cloud Logging + Cloud Trace integration.
**Cross-cloud:** `aiinfra-learning-pod/screen1-securing-cross-cloud-agentic` — securing cross-cloud agentic workflows.

**Trogonai:** NATS subjects provide routing. No rate limiting, no canary deployments, no IAP, no mTLS agent identity, no semantic policy enforcement at the gateway.

**Gap:** Trogonai has message routing but not governed, authenticated, policy-enforced traffic management.

---

### 3.5 Model Armor

Content security screening integrated into Agent Gateway:

**What it screens:**
- Prompt injection attempts
- Jailbreak attempts
- Sensitive information leakage
- Harmful content generation

**Supported traffic protocols:**

| Traffic type | Protocol | What is screened |
|---|---|---|
| Client-to-Agent | ADK protocol | `reasoningEngines.streamQuery` requests/responses |
| Agent-to-Agent | A2A v1 | Send Message, Agent Card, JSON-RPC, HTTP+JSON/REST |
| Agent-to-MCP | MCP | `tools/call`, `prompts/get` requests/responses |
| Agent-to-LLM | OpenAI API | Chat completions, embeddings, messages, threads |

**Configuration:**
```bash
# 1. Enable API
https://console.cloud.google.com/flows/enableapi?apiid=modelarmor.googleapis.com

# 2. Create template (safety filters, thresholds, enforcement type)
# Required role: roles/modelarmor.admin

# 3. Set enforcement on Agent Gateway: INSPECT_ONLY or INSPECT_AND_BLOCK

# 4. Enable redaction with DLP de-identify templates
```

**IAM roles:** `roles/modelarmor.admin` (templates), `roles/modelarmor.calloutUser` (runtime), `roles/modelarmor.user`.

Security findings: `govern/view-security-findings`, `govern/monitor-content-security`, `govern/view-model-armor-spans`.

**Trogonai:** No Model Armor equivalent. No prompt injection detection. No harmful content screening at any layer.

**Gap:** Trogonai has no defense-in-depth content security layer.

---

## SECTION 4: OPTIMIZE

### 4.1 Evaluation

**Three evaluation modes:**

**Offline** — fixed evalsets, automated scoring:
```json
// test.evalset.json
{ "eval_set_id": "ticket_tests", "eval_cases": [{
    "eval_id": "escalation_test",
    "conversation": [{
        "user_content": {"parts": [{"text": "Payment failed for 3 days"}]},
        "final_response": {"parts": [{"text": "Escalated to tier-2"}]},
        "intermediate_data": { "tool_uses": [{"tool_name": "escalate_ticket"}] }
    }]
}]}
```

```bash
adk eval agent.py evals/test.evalset.json --config_file_path=config.json --print_detailed_results
```

**Simulated** — AI-generated user personas, multi-turn synthetic scenarios:
- Automatically generates diverse test scenarios from agent instructions and tool definitions
- Environment simulation: intercept tool calls, inject mocked data, simulate errors (HTTP 503, latency spikes, custom responses) — tests resilience without real external calls
- Multi-turn autoraters evaluate entire conversation histories
- Prompt optimization: programmatically generate and validate refined system instructions; identify failure points and propose targeted updates

**Online** — continuous production monitoring:
- Samples live production traffic automatically
- Scores quality in real time using the same metrics as offline evaluation
- Quality alerts: configure thresholds on any metric; receive alerts when quality drops

**Built-in metrics:**

| Metric | Type | Measures |
|---|---|---|
| `tool_trajectory_avg_score` | Reference-based | Right tools in the right order? |
| `response_match_score` | Reference-based | ROUGE-1 similarity to expected response |
| `final_response_match_v2` | Reference-based | Semantic equivalence |
| `hallucinations_v1` | Reference-free | Is response grounded in provided context? |
| `safety_v1` | Reference-free | Does response violate safety policies? |
| `multi_turn_task_success_v1` | Reference-free | Goal completion across turns |
| `rubric_based_final_response_quality_v1` | Reference-free | Custom quality rubric |
| `Helpfulness` | Reference-free | Is the response helpful? |
| `Task Success` | Reference-free | Did the agent accomplish the task? |
| `Exact Match` | Reference-based | Exact string match |

Sub-pages: `evaluate-agents`, `evaluate-offline`, `evaluate-simulated`, `evaluate-online`, `manage-metrics`, `view-results`, `quality-alerts`, `optimize-agent`.

**Trogonai:** `trogon-e2e` tests infrastructure behavior only (messages delivered, services respond). No quality metrics, no trajectory scoring, no hallucination detection, no simulated evaluation, no online monitoring, no quality alerts.

**Gap:** Trogonai cannot evaluate agent quality in any form. Most critical enterprise gap — no agent ships to production without a quality baseline.

---

### 4.2 Observability and Topology

**Tracing:** OpenTelemetry-compatible, Cloud Trace integration. Distributed trace spans: event ingestion → routing → tool calls → model calls → response. Sub-page: `observability/traces`.

**Topology visualization** (`observability/topology`):
- Graph view of agent-to-agent and agent-to-MCP relationships across the entire deployment
- Derived from aggregated trace data across discovered agents and Agent Registry entries
- Interactive: zoom, drag nodes, click for detail panels
- Table view: filterable, sortable, customizable columns
- Use cases: understand dependencies, support governance, troubleshoot communication issues, monitor multi-agent interactions at scale

**Logging:** Cloud Logging integration, structured per-agent, per-session, per-tool. Sub-page: `scale/runtime/logging`.

**Monitoring:** Cloud Monitoring dashboards, custom metrics per agent, per model, per tool. Sub-page: `scale/runtime/monitoring`.

**Trogonai:** `acp-telemetry` provides OTel setup (traces, metrics, logs) with OTLP export — vendor-neutral (Grafana, Jaeger, Datadog compatible). Trace context propagates through NATS message headers across the full pipeline. Strong infrastructure observability. Missing: topology visualization, agent-quality dashboards, agent observability platforms (AgentOps, Arize, Galileo, Phoenix, LangWatch).

**Partial parity:** OTel traces exist and are well-implemented. Topology and agent-quality analytics are missing.

---

### 4.3 Example Store

Managed repository for few-shot examples that improve agent quality without redeployment:

**Workflow:**
1. **Create** an `ExampleStore` resource (max 50 per region/project)
2. **Upload examples** — two types:
   - Correct examples: when agent response matches expectations
   - Corrective examples: demonstrate the right response when agent made a mistake
3. **Retrieve** — for ADK agents: automatic; agent linked to Example Store auto-retrieves relevant examples at inference time. For non-ADK: manual via cosine similarity search + function name filtering
4. **Effect**: examples inject immediately without redeployment; agent quality improves in production without code changes

Sub-pages: `create-examplestore`, `upload-examples`, `retrieve-examples`.

**Trogonai:** No Example Store. No few-shot example management. Agent prompt improvement requires code changes and redeployment.

**Gap:** Trogonai has no mechanism for iterative quality improvement without code changes.

---

## Summary Tables

### BUILD layer

| Capability | Platform API | Trogonai |
|---|---|---|
| Agent definition | `LlmAgent(model, tools, instruction, ...)` — 4 languages | Rust trait only |
| SequentialAgent | `SequentialAgent(sub_agents=[...])` + `output_key` state flow | Not present |
| ParallelAgent | `ParallelAgent(sub_agents=[...])` | Not present |
| LoopAgent | `LoopAgent(sub_agents=[...], max_iterations=N)` + `escalate` stop | Not present |
| Inline function tool | 5-line annotated function → auto-schema | MCP server required |
| AgentTool | `AgentTool(agent=...)` | Not present |
| LongRunningFunctionTool | `LongRunningFunctionTool(func=...)` | Not present (vault approvals only) |
| OpenAPI tool generation | Auto-generates from spec | Not present |
| MCP tools | `McpToolset(StdioConnectionParams / StreamableHTTPConnectionParams)` | `trogon-mcp` — **parity** |
| Callbacks (6 hooks) | `before/after_agent/model/tool_callback` | Not present |
| Session SDK | `create/get/list/delete_session()` — 3 backends | ACP messages only |
| Session REST API | `POST/GET/DELETE .../sessions`, `POST .../sessions/{id}/events` | Not present |
| Memory `search_memory()` | `VertexAiMemoryBankService` + `PreloadMemoryTool` | Not present |
| `GenerateMemories` REST | `client.agent_engines.memories.generate(...)` | Not present |
| Context compaction | Built-in | `trogon-compactor` — **parity** |
| Artifact management | `save/load/list_artifact()` — versioned, GCS-backed | Not present |
| Runner + event stream | `runner.run_async(user_id, session_id, message)` | NATS pub/sub only |
| Local dev server | `adk web` — UI + trace viewer | Not present |
| ToolContext APIs | `search_memory`, `request_credential`, `list_artifacts` | Not present |
| A2A protocol | Open standard — expose + consume — cross-framework | Not present |
| RAG Engine | Managed 6-step pipeline, 7 vector DB backends, multimodal | Not present |
| Vector Search 2.0 | Collections, auto-tuning, hybrid search | Not present |
| Google Search grounding | Built-in tool | Not present |
| Agent Studio | Visual no-code agent design UI | Not present |
| Agent Garden | Pre-built templates (RAG, ReAct, multi-agent) | Not present |
| 3-legged OAuth | `request_credential()` + consent flow | Not present |

### SCALE layer

| Capability | Platform API | Trogonai |
|---|---|---|
| Managed auto-scaling runtime | `adk deploy agent_engine` → REST query API | Self-hosted NATS + binaries |
| Multi-framework runtime | ADK, LangChain, LangGraph, AG2, LlamaIndex, CrewAI | Single framework (Rust/ACP) |
| Unified model API (100+ models) | `LlmAgent(model="claude-3-5-sonnet")` | One runner binary per model |
| Memory Bank + Profiles + Revisions | `GenerateMemories`, `FetchMemories`, `IngestEvents` | Not present |
| Code execution sandbox | `BuiltInCodeExecutor()` / `GkeSandboxCodeExecutor()` | `trogon-wasm-runtime` — **parity** |
| Bidirectional streaming | Built-in | Not present |

### GOVERN layer

| Capability | Platform API | Trogonai |
|---|---|---|
| Model safety filters | `SafetySetting(category, threshold)` | Not present |
| DLP / PII redaction | DLP API — redact, mask, tokenize, custom infoTypes | Not present |
| Gemini-as-Filter | Secondary model policy evaluation, multimodal | Not present |
| Semantic Governance Policies | `gcloud ai-platform semanticGovernancePolicy create` — NLC rules, ALLOW/DENY/ALLOW_IF_CONFIRMED | Not present |
| Agent Registry | Register agents + MCP servers + endpoints; discovery; auth binding | Runtime discovery only |
| Agent Gateway (ingress + egress) | IAP + IAM + Model Armor + SGP + mTLS; Client-to-Agent + Agent-to-Anywhere | NATS routing only |
| Model Armor | Screens ADK / A2A / MCP / OpenAI traffic; INSPECT_ONLY or INSPECT_AND_BLOCK | Not present |
| Security findings | `view-security-findings`, Security Command Center integration | Not present |
| Credential isolation (token proxy) | Not present | `trogon-secret-proxy` — **Trogonai ahead** |

### OPTIMIZE layer

| Capability | Platform API | Trogonai |
|---|---|---|
| Offline evaluation + metrics (10) | `adk eval` + `.evalset.json` + `hallucinations_v1`, `safety_v1`, etc. | Not present |
| Simulated evaluation + env simulation | AI-generated personas; inject mock tool responses, HTTP 503, latency spikes | Not present |
| Prompt optimization | Auto-generate refined system instructions programmatically | Not present |
| Online evaluation + quality alerts | Continuous production sampling + threshold-based alerts | Not present |
| Example Store | `ExampleStore`; upload + auto-retrieve; no redeployment needed | Not present |
| Distributed tracing | OTel + Cloud Trace | `acp-telemetry` (OTel) — **parity** |
| Topology visualization | Agent-to-agent + agent-to-MCP graph; interactive; table view | Not present |
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

Trogonai is a strong **infrastructure platform** — event ingestion, credential isolation, job scheduling, NATS transport, and feature flags are capabilities the Gemini Enterprise Agent Platform does not provide and deliberately delegates to the broader GCP ecosystem. In these areas Trogonai is genuinely ahead.

The gap is in the **agent authoring, runtime, governance, and quality layers**. In priority order to close it:

1. **Evaluation framework** — no agent ships to production without a quality baseline; hallucination detection and trajectory scoring are minimum enterprise requirements
2. **REST control plane** — merge `feat/console-api`; non-Rust developers need HTTP access to sessions and agent invocation
3. **Callback / middleware system** — `before/after_model` and `before/after_tool` hooks on the actor lifecycle; unblocks safety, PII redaction, and governance enforcement
4. **Workflow agent primitives** — `SequentialActor`, `ParallelActor`, `LoopActor` as NATS-native patterns
5. **Semantic memory** — `search_memory` API backed by a vector store; cross-session knowledge retrieval
6. **Unified model API** — single dispatch interface instead of one runner binary per model
7. **A2A protocol adoption** — cross-framework agent interoperability
8. **Topology observability** — agent-to-agent call graph derived from existing OTel traces
