//! End-to-end tests: trogon-console ↔ trogon-xai-runner round-trip.
//!
//! Tests that require `XAI_API_KEY` are skipped silently when it is not set.
//! HTTP tests use a real NATS container + axum server but no xAI API calls.

use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::{
    Agent as _, CloseSessionRequest, ContentBlock, NewSessionRequest, PromptRequest,
    SessionNotification, StopReason,
};
use async_nats::jetstream;
use async_trait::async_trait;
use testcontainers_modules::{
    nats::Nats,
    testcontainers::{ImageExt, runners::AsyncRunner},
};
use trogon_console::{
    models::agent::{AgentDefinition, AgentModel, AgentStatus},
    models::skill::{Skill, SkillVersion},
    server::{AppState, build_router},
    store::{
        agents::AgentStore,
        credentials::CredentialStore,
        environments::EnvironmentStore,
        sessions::SessionReader,
        skills::SkillStore,
        traits::{
            AgentRepository, CredentialRepository, EnvironmentRepository, SessionRepository,
            SkillRepository,
        },
    },
};
use trogon_xai_runner::{
    AgentLoader, SessionNotifier, SkillLoader, XaiAgent, XaiClient,
    session_store::{NatsSessionStore, SessionStoring},
};

// ── Shared helpers ────────────────────────────────────────────────────────────

async fn make_js() -> (jetstream::Context, impl Drop) {
    let container = Nats::default()
        .with_cmd(["--jetstream"])
        .start()
        .await
        .expect("start NATS container");
    let port = container.get_host_port_ipv4(4222).await.expect("port");
    let nats = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .expect("connect to NATS");
    (jetstream::new(nats), container)
}

struct NoOpNotifier;

#[async_trait(?Send)]
impl SessionNotifier for NoOpNotifier {
    async fn notify(&self, _: SessionNotification) {}
}

/// Seed CONSOLE_AGENTS + CONSOLE_SKILLS + CONSOLE_SKILL_VERSIONS via real stores.
async fn seed_agent_with_skills(
    js: &jetstream::Context,
    agent_id: &str,
    skill_specs: &[(&str, &str, &str)], // (skill_id, name, content)
) {
    let agent_store = AgentStore::open(js).await.unwrap();
    let skill_store = SkillStore::open(js).await.unwrap();
    let now = "1745000000".to_string();
    let ver = "20260421".to_string();

    for (skill_id, name, content) in skill_specs {
        skill_store
            .put(&Skill {
                id: skill_id.to_string(),
                name: name.to_string(),
                description: String::new(),
                provider: "custom".to_string(),
                latest_version: ver.clone(),
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .await
            .unwrap();
        skill_store
            .put_version(&SkillVersion {
                skill_id: skill_id.to_string(),
                version: ver.clone(),
                content: content.to_string(),
                is_latest: true,
                created_at: now.clone(),
            })
            .await
            .unwrap();
    }

    let skill_ids = skill_specs.iter().map(|(id, _, _)| id.to_string()).collect();
    agent_store
        .put(&AgentDefinition {
            id: agent_id.to_string(),
            name: "E2E Agent".to_string(),
            description: String::new(),
            status: AgentStatus::Active,
            version: 1,
            model: AgentModel {
                id: "grok-3-mini".to_string(),
                speed: "standard".to_string(),
            },
            system_prompt: "You are a helpful assistant.".to_string(),
            skill_ids,
            tools: vec![],
            mcp_servers: vec![],
            metadata: serde_json::Value::Null,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .await
        .unwrap();
}

/// Build an `XaiAgent` wired to real NATS loaders and session store.
async fn build_xai_agent(
    js: &jetstream::Context,
    agent_id: &str,
    api_key: &str,
) -> XaiAgent<XaiClient, NoOpNotifier> {
    let agent_loader = AgentLoader::open(js).await.unwrap();
    let skill_loader = SkillLoader::open(js).await.unwrap();
    let session_store = NatsSessionStore::open(js).await.unwrap();

    XaiAgent::with_deps(NoOpNotifier, "grok-3-mini", api_key, XaiClient::new())
        .with_loaders(agent_id, Arc::new(agent_loader), Arc::new(skill_loader))
        .with_session_store(Arc::new(session_store) as Arc<dyn SessionStoring>)
}

/// Start a real trogon-console HTTP server; returns (reqwest client, base URL, task handle).
async fn start_console_http(
    js: &jetstream::Context,
) -> (reqwest::Client, String, tokio::task::JoinHandle<()>) {
    let state = Arc::new(AppState {
        agents: Arc::new(AgentStore::open(js).await.unwrap()) as Arc<dyn AgentRepository>,
        skills: Arc::new(SkillStore::open(js).await.unwrap()) as Arc<dyn SkillRepository>,
        environments: Arc::new(EnvironmentStore::open(js).await.unwrap())
            as Arc<dyn EnvironmentRepository>,
        credentials: Arc::new(CredentialStore::open(js).await.unwrap())
            as Arc<dyn CredentialRepository>,
        sessions: Arc::new(SessionReader::open(js).await.unwrap()) as Arc<dyn SessionRepository>,
    });

    let router = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.ok(); });
    tokio::time::sleep(Duration::from_millis(80)).await;

    (reqwest::Client::new(), format!("http://{addr}"), handle)
}

// ── Test 1: Basic round-trip ──────────────────────────────────────────────────

/// Creates agent + skill via console stores, runs a real xAI prompt, reads back
/// the session via SessionReader and verifies agent_id, tokens, message count.
#[tokio::test]
async fn xai_agent_console_end_to_end() {
    let api_key = match std::env::var("XAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => { eprintln!("XAI_API_KEY not set — skipping"); return; }
    };

    let (js, _c) = make_js().await;
    let agent_id = "e2e-agent-001";
    seed_agent_with_skills(
        &js,
        agent_id,
        &[("concise-helper", "Concise Helper",
           "Always reply with the shortest possible answer.")],
    ).await;

    let session_reader = SessionReader::open(&js).await.unwrap();
    let agent = build_xai_agent(&js, agent_id, &api_key).await;

    let local = tokio::task::LocalSet::new();
    let (session_id, stop_reason) = local.run_until(async move {
        let resp = agent.new_session(NewSessionRequest::new("/tmp")).await.unwrap();
        let sid = resp.session_id.to_string();
        let pr = agent.prompt(PromptRequest::new(sid.clone(),
            vec![ContentBlock::from("What is 2+2?")])).await.unwrap();
        (sid, pr.stop_reason)
    }).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let s = session_reader.get("default", &session_id).await.unwrap()
        .expect("session must be in SESSIONS bucket");

    assert_eq!(stop_reason, StopReason::EndTurn);
    assert_eq!(s.agent_id.as_deref(), Some(agent_id));
    assert!(s.message_count >= 2);
    assert!(s.output_tokens > 0, "output_tokens must be > 0");
    assert_eq!(s.tenant_id, "default");
    assert!(!s.name.is_empty());
}

// ── Test 2: Multi-turn, model from console, close_session, session listing ────

/// Two prompts + close_session. Verifies model taken from console AgentDefinition,
/// input+output tokens, message count, Idle status, and SessionReader.list().
#[tokio::test]
async fn xai_multi_turn_model_and_close() {
    let api_key = match std::env::var("XAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => { eprintln!("XAI_API_KEY not set — skipping"); return; }
    };

    let (js, _c) = make_js().await;
    let agent_id = "multi-turn-agent";
    seed_agent_with_skills(&js, agent_id,
        &[("brevity", "Brevity", "Keep all answers under 10 words.")]).await;

    let session_reader = SessionReader::open(&js).await.unwrap();
    let agent = build_xai_agent(&js, agent_id, &api_key).await;

    let local = tokio::task::LocalSet::new();
    let session_id = local.run_until(async move {
        let resp = agent.new_session(NewSessionRequest::new("/tmp")).await.unwrap();
        let sid = resp.session_id.to_string();

        agent.prompt(PromptRequest::new(sid.clone(),
            vec![ContentBlock::from("What is 2+2?")])).await.unwrap();
        agent.prompt(PromptRequest::new(sid.clone(),
            vec![ContentBlock::from("And 3+3?")])).await.unwrap();

        agent.close_session(CloseSessionRequest::new(sid.clone())).await.unwrap();
        sid
    }).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    let s = session_reader.get("default", &session_id).await.unwrap()
        .expect("session must survive close_session");

    // Model taken from console AgentDefinition.model.id
    assert_eq!(s.model.as_deref(), Some("grok-3-mini"),
        "model must match AgentDefinition written by trogon-console");
    // Two user + two assistant = 4 messages
    assert_eq!(s.message_count, 4, "expected 4 messages after 2 turns");
    assert!(s.input_tokens > 0, "input_tokens must be summed from usage events");
    assert!(s.output_tokens > 0, "output_tokens must be summed from usage events");
    // Last message is assistant → status == Idle
    assert_eq!(s.status, trogon_console::models::session::SessionStatus::Idle,
        "status must be Idle when last message is from assistant");
    // session name derived from first user message
    assert!(s.name.to_lowercase().contains("2+2") || !s.name.is_empty());

    // Session visible in list()
    let all = session_reader.list().await.unwrap();
    assert!(all.iter().any(|x| x.id == session_id),
        "session must appear in SessionReader::list()");
}

// ── Test 3: Multiple skills concatenated and injected ─────────────────────────

/// Two skills injected simultaneously. Verifies the SkillLoader concatenates
/// both and the model follows the combined instruction (marker in response).
#[tokio::test]
async fn xai_multiple_skills_injected() {
    let api_key = match std::env::var("XAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => { eprintln!("XAI_API_KEY not set — skipping"); return; }
    };

    let (js, _c) = make_js().await;
    let agent_id = "multi-skill-agent";
    seed_agent_with_skills(&js, agent_id, &[
        ("marker-skill", "Marker",
         "You MUST include the exact text TROGON_SKILL_OK somewhere in every response."),
        ("brevity-skill", "Brevity",
         "Keep responses under 20 words."),
    ]).await;

    let session_reader = SessionReader::open(&js).await.unwrap();
    let agent = build_xai_agent(&js, agent_id, &api_key).await;

    let local = tokio::task::LocalSet::new();
    let session_id = local.run_until(async move {
        let resp = agent.new_session(NewSessionRequest::new("/tmp")).await.unwrap();
        let sid = resp.session_id.to_string();
        agent.prompt(PromptRequest::new(sid.clone(),
            vec![ContentBlock::from("What is 2+2?")])).await.unwrap();
        sid
    }).await;

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Read the raw snapshot from SESSIONS KV to inspect message text
    let sessions_kv = js.get_key_value("SESSIONS").await
        .expect("SESSIONS bucket must exist");
    let bytes = sessions_kv.get(&format!("default.{session_id}")).await
        .unwrap().expect("session must be in bucket");
    let raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let assistant_text = raw["messages"][1]["content"][0]["text"]
        .as_str()
        .unwrap_or("");

    assert!(
        assistant_text.contains("TROGON_SKILL_OK"),
        "skill marker 'TROGON_SKILL_OK' not found in assistant response: {:?}",
        assistant_text
    );

    // Also verify session appears in listing
    let all = session_reader.list().await.unwrap();
    assert!(all.iter().any(|x| x.id == session_id));
}

// ── Test 4: Console HTTP — session endpoints with real NATS data ──────────────

/// Starts the real trogon-console HTTP server against real NATS. Pre-seeds the
/// SESSIONS bucket via NatsSessionStore, then verifies GET /sessions,
/// GET /sessions/{tenant}/{id}, and GET /agents/{id}/sessions (agent_id filter).
#[tokio::test]
async fn console_http_sessions_endpoints() {
    let (js, _c) = make_js().await;

    // Pre-seed SESSIONS bucket with two snapshots: one linked to agent-a, one not.
    let store = NatsSessionStore::open(&js).await.unwrap();
    use trogon_xai_runner::session_store::{SessionSnapshot, SessionStoring, SnapshotMessage, TextBlock};
    let now = "2026-04-21T00:00:00.000Z";

    // snap_a: belongs to agent-a via agent_id field; tenant_id is independent.
    let snap_a = SessionSnapshot {
        id: "sess-agent-a".to_string(),
        tenant_id: "default".to_string(),
        name: "Session for agent-a".to_string(),
        model: Some("grok-3-mini".to_string()),
        tools: vec![],
        memory_path: None,
        agent_id: Some("agent-a".to_string()),
        messages: vec![
            SnapshotMessage { role: "user".into(),
                content: vec![TextBlock::new("Hello")], usage: None },
            SnapshotMessage { role: "assistant".into(),
                content: vec![TextBlock::new("Hi!")], usage: None },
        ],
        created_at: now.to_string(),
        updated_at: now.to_string(),
    };
    let snap_b = SessionSnapshot {
        id: "sess-no-agent".to_string(),
        tenant_id: "default".to_string(),
        name: "Session without agent".to_string(),
        model: Some("grok-3-mini".to_string()),
        tools: vec![],
        memory_path: None,
        agent_id: None,
        messages: vec![
            SnapshotMessage { role: "user".into(),
                content: vec![TextBlock::new("Hey")], usage: None },
        ],
        created_at: now.to_string(),
        updated_at: now.to_string(),
    };

    store.save(&snap_a).await;
    store.save(&snap_b).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create agent-a in console so /agents/agent-a/sessions works
    let agent_store = AgentStore::open(&js).await.unwrap();
    agent_store.put(&AgentDefinition {
        id: "agent-a".to_string(),
        name: "Agent A".to_string(),
        description: String::new(),
        status: AgentStatus::Active,
        version: 1,
        model: AgentModel { id: "grok-3-mini".to_string(), speed: "standard".to_string() },
        system_prompt: String::new(),
        skill_ids: vec![],
        tools: vec![],
        mcp_servers: vec![],
        metadata: serde_json::Value::Null,
        created_at: "1745000000".to_string(),
        updated_at: "1745000000".to_string(),
    }).await.unwrap();

    let (http, base, _handle) = start_console_http(&js).await;

    // GET /sessions → both sessions appear
    let resp = http.get(&format!("{base}/sessions")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let ids: Vec<&str> = body.as_array().unwrap()
        .iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"sess-agent-a"), "sess-agent-a missing from /sessions");
    assert!(ids.contains(&"sess-no-agent"), "sess-no-agent missing from /sessions");

    // GET /sessions/{tenant}/{id} → specific session (tenant_id = "default")
    let resp = http.get(&format!("{base}/sessions/default/sess-agent-a")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["id"], "sess-agent-a");
    assert_eq!(body["agent_id"], "agent-a");
    assert_eq!(body["message_count"], 2);

    // GET /sessions/default/missing → 404
    let resp = http.get(&format!("{base}/sessions/default/no-such")).send().await.unwrap();
    assert_eq!(resp.status(), 404);

    // GET /agents/agent-a/sessions → filters by agent_id field, not tenant_id
    let resp = http.get(&format!("{base}/agents/agent-a/sessions")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let agent_session_ids: Vec<&str> = body.as_array().unwrap()
        .iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert!(agent_session_ids.contains(&"sess-agent-a"),
        "sess-agent-a must appear (agent_id == 'agent-a')");
    assert!(!agent_session_ids.contains(&"sess-no-agent"),
        "sess-no-agent must NOT appear (agent_id is None)");
}

// ── Test 5: Console HTTP — agent + skill CRUD with real NATS ─────────────────

/// Full CRUD cycle through the real HTTP API backed by real NATS JetStream.
/// Verifies that what the HTTP layer writes can be read back correctly.
#[tokio::test]
async fn console_http_agent_skill_crud() {
    let (js, _c) = make_js().await;
    let (http, base, _handle) = start_console_http(&js).await;

    // ── Health ────────────────────────────────────────────────────────────────
    let resp = http.get(&format!("{base}/-/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // ── POST /agents ──────────────────────────────────────────────────────────
    let resp = http.post(&format!("{base}/agents"))
        .json(&serde_json::json!({
            "name": "Test Agent",
            "description": "e2e crud",
            "model": { "id": "grok-3-mini" },
            "system_prompt": "Be helpful.",
            "skill_ids": []
        }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let agent: serde_json::Value = resp.json().await.unwrap();
    let agent_id = agent["id"].as_str().unwrap().to_string();
    assert_eq!(agent["name"], "Test Agent");
    assert_eq!(agent["version"], 1);

    // ── GET /agents → list includes created agent ─────────────────────────────
    let resp = http.get(&format!("{base}/agents")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.unwrap();
    let found = list.as_array().unwrap().iter().any(|a| a["id"] == agent_id);
    assert!(found, "created agent must appear in GET /agents");

    // ── GET /agents/{id} ──────────────────────────────────────────────────────
    let resp = http.get(&format!("{base}/agents/{agent_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let got: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(got["id"], agent_id.as_str());

    // ── PUT /agents/{id} → version increments ────────────────────────────────
    let resp = http.put(&format!("{base}/agents/{agent_id}"))
        .json(&serde_json::json!({ "name": "Updated Agent" }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["name"], "Updated Agent");
    assert_eq!(updated["version"], 2, "version must increment on update");

    // ── GET /agents/{id}/versions → history ──────────────────────────────────
    let resp = http.get(&format!("{base}/agents/{agent_id}/versions")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let versions: serde_json::Value = resp.json().await.unwrap();
    assert!(versions.as_array().unwrap().len() >= 2, "must have at least 2 version entries");

    // ── POST /skills ──────────────────────────────────────────────────────────
    let resp = http.post(&format!("{base}/skills"))
        .json(&serde_json::json!({
            "name": "My Skill",
            "description": "test skill",
            "content": "You are an expert."
        }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let skill: serde_json::Value = resp.json().await.unwrap();
    let skill_id = skill["id"].as_str().unwrap().to_string();
    assert_eq!(skill["name"], "My Skill");

    // ── GET /skills/{id} ─────────────────────────────────────────────────────
    let resp = http.get(&format!("{base}/skills/{skill_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // ── POST /skills/{id}/versions → new version ──────────────────────────────
    let resp = http.post(&format!("{base}/skills/{skill_id}/versions"))
        .json(&serde_json::json!({ "content": "Updated skill content." }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let v2: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v2["skill_id"], skill_id.as_str());

    // ── GET /skills/{id}/versions ─────────────────────────────────────────────
    let resp = http.get(&format!("{base}/skills/{skill_id}/versions")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let skill_versions: serde_json::Value = resp.json().await.unwrap();
    assert!(skill_versions.as_array().unwrap().len() >= 1, "must have at least 1 version");

    // ── DELETE /agents/{id} ───────────────────────────────────────────────────
    let resp = http.delete(&format!("{base}/agents/{agent_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 204);
    let resp = http.get(&format!("{base}/agents/{agent_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 404, "deleted agent must return 404");

    // ── GET /skills (list) ────────────────────────────────────────────────────
    let resp = http.get(&format!("{base}/skills")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.unwrap();
    assert!(list.as_array().unwrap().iter().any(|s| s["id"] == skill_id.as_str()),
        "skill must appear in GET /skills list");

    // ── DELETE /skills/{id} ───────────────────────────────────────────────────
    let resp = http.delete(&format!("{base}/skills/{skill_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 204);
    let resp = http.get(&format!("{base}/skills/{skill_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 404, "deleted skill must return 404");
}

// ── Test 6: Environments CRUD with real NATS ──────────────────────────────────

#[tokio::test]
async fn console_http_environments_crud() {
    let (js, _c) = make_js().await;
    let (http, base, _handle) = start_console_http(&js).await;

    // POST /environments
    let resp = http.post(&format!("{base}/environments"))
        .json(&serde_json::json!({
            "name": "Prod Cloud",
            "description": "production env",
            "type": "cloud",
            "networking": "unrestricted",
            "packages": [{ "manager": "pip", "spec": "requests==2.31.0" }]
        }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let env: serde_json::Value = resp.json().await.unwrap();
    let env_id = env["id"].as_str().unwrap().to_string();
    assert_eq!(env["name"], "Prod Cloud");
    assert_eq!(env["type"], "cloud");
    assert_eq!(env["archived"], false);

    // GET /environments → list includes it
    let resp = http.get(&format!("{base}/environments")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.unwrap();
    assert!(list.as_array().unwrap().iter().any(|e| e["id"] == env_id.as_str()));

    // GET /environments/{id}
    let resp = http.get(&format!("{base}/environments/{env_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let got: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(got["id"], env_id.as_str());
    assert_eq!(got["packages"][0]["manager"], "pip");

    // PUT /environments/{id} → update name and networking
    let resp = http.put(&format!("{base}/environments/{env_id}"))
        .json(&serde_json::json!({ "name": "Prod Cloud Updated", "networking": "restricted" }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let updated: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(updated["name"], "Prod Cloud Updated");
    assert_eq!(updated["networking"], "restricted");

    // POST /environments/{id}/archive
    let resp = http.post(&format!("{base}/environments/{env_id}/archive"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let archived: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(archived["archived"], true, "archived flag must be true after archive");

    // DELETE /environments/{id}
    let resp = http.delete(&format!("{base}/environments/{env_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 204);
    let resp = http.get(&format!("{base}/environments/{env_id}")).send().await.unwrap();
    assert_eq!(resp.status(), 404, "deleted environment must return 404");
}

// ── Test 7: Credentials + Vaults with real NATS ───────────────────────────────

#[tokio::test]
async fn console_http_credentials_and_vaults() {
    let (js, _c) = make_js().await;
    let (http, base, _handle) = start_console_http(&js).await;

    // Create an environment first (credentials belong to environments)
    let resp = http.post(&format!("{base}/environments"))
        .json(&serde_json::json!({ "name": "Cred Env", "description": "" }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let env: serde_json::Value = resp.json().await.unwrap();
    let env_id = env["id"].as_str().unwrap().to_string();

    // GET /environments/{id}/vault → auto-created on first credential
    // (vault doesn't exist yet — expect 404)
    let resp = http.get(&format!("{base}/environments/{env_id}/vault")).send().await.unwrap();
    assert_eq!(resp.status(), 404, "vault must not exist before first credential");

    // POST /environments/{id}/credentials → auto-creates vault
    let resp = http.post(&format!("{base}/environments/{env_id}/credentials"))
        .json(&serde_json::json!({
            "name": "GitHub Token",
            "type": "bearer_token",
            "mcp_server_url": "https://mcp.github.com/mcp"
        }))
        .send().await.unwrap();
    assert_eq!(resp.status(), 201);
    let cred: serde_json::Value = resp.json().await.unwrap();
    let cred_id = cred["id"].as_str().unwrap().to_string();
    assert_eq!(cred["name"], "GitHub Token");
    assert_eq!(cred["type"], "bearer_token");
    assert_eq!(cred["status"], "active");

    // GET /environments/{id}/vault → now exists
    let resp = http.get(&format!("{base}/environments/{env_id}/vault")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let vault: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(vault["env_id"], env_id.as_str());

    // GET /environments/{id}/credentials → list includes the credential
    let resp = http.get(&format!("{base}/environments/{env_id}/credentials")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let creds: serde_json::Value = resp.json().await.unwrap();
    assert!(creds.as_array().unwrap().iter().any(|c| c["id"] == cred_id.as_str()));

    // GET /environments/{id}/credentials/{cred_id}
    let resp = http.get(&format!("{base}/environments/{env_id}/credentials/{cred_id}"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let got: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(got["id"], cred_id.as_str());
    assert_eq!(got["mcp_server_url"], "https://mcp.github.com/mcp");

    // DELETE /environments/{id}/credentials/{cred_id}
    let resp = http.delete(&format!("{base}/environments/{env_id}/credentials/{cred_id}"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 204);
    let resp = http.get(&format!("{base}/environments/{env_id}/credentials/{cred_id}"))
        .send().await.unwrap();
    assert_eq!(resp.status(), 404, "deleted credential must return 404");
}

// ── Test 8: MCP Registry ──────────────────────────────────────────────────────

#[tokio::test]
async fn console_http_mcp_registry() {
    let (js, _c) = make_js().await;
    let (http, base, _handle) = start_console_http(&js).await;

    let resp = http.get(&format!("{base}/mcp-registry")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let list: serde_json::Value = resp.json().await.unwrap();
    let servers = list.as_array().unwrap();

    assert_eq!(servers.len(), 10, "must return all 10 known MCP servers");

    let names: Vec<&str> = servers.iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"GitHub"));
    assert!(names.contains(&"Slack"));
    assert!(names.contains(&"Linear"));

    // Every entry must have a non-empty url
    for s in servers {
        assert!(s["url"].as_str().unwrap_or("").starts_with("https://"),
            "MCP server url must be https: {:?}", s["url"]);
    }
}

// ── Test 9: Session status=Running ───────────────────────────────────────────

/// Seeds a session whose last message is from `user` (simulates a cancelled or
/// in-progress turn) and verifies trogon-console derives status=Running.
#[tokio::test]
async fn console_session_status_running() {
    let (js, _c) = make_js().await;

    let store = NatsSessionStore::open(&js).await.unwrap();
    use trogon_xai_runner::session_store::{SessionSnapshot, SessionStoring, SnapshotMessage, TextBlock};
    let now = "2026-04-21T00:00:00.000Z";

    // Last message is "user" → status must be Running
    store.save(&SessionSnapshot {
        id: "sess-running".to_string(),
        tenant_id: "default".to_string(),
        name: "Pending session".to_string(),
        model: Some("grok-3-mini".to_string()),
        tools: vec![],
        memory_path: None,
        agent_id: None,
        messages: vec![
            SnapshotMessage { role: "user".into(),
                content: vec![TextBlock::new("Hello")], usage: None },
            SnapshotMessage { role: "assistant".into(),
                content: vec![TextBlock::new("Hi!")], usage: None },
            SnapshotMessage { role: "user".into(),
                content: vec![TextBlock::new("Follow-up question")], usage: None },
        ],
        created_at: now.to_string(),
        updated_at: now.to_string(),
    }).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let (http, base, _handle) = start_console_http(&js).await;

    let resp = http.get(&format!("{base}/sessions/default/sess-running")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();

    assert_eq!(body["status"], "running",
        "status must be 'running' when last message role is 'user'");
    assert_eq!(body["message_count"], 3);
}
