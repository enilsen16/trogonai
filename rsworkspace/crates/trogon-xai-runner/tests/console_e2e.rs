//! End-to-end test: trogon-console creates an agent + skill, xAI runner reads
//! the config, makes a real API call, persists the session, and trogon-console
//! reads it back from the SESSIONS bucket.
//!
//! Requires `XAI_API_KEY` to be set; skipped silently otherwise.

use std::sync::Arc;

use agent_client_protocol::{
    Agent as _, ContentBlock, NewSessionRequest, PromptRequest, SessionNotification, StopReason,
};
use async_trait::async_trait;
use testcontainers_modules::{
    nats::Nats,
    testcontainers::{ImageExt, runners::AsyncRunner},
};
use trogon_console::{
    models::agent::{AgentDefinition, AgentModel, AgentStatus},
    models::skill::{Skill, SkillVersion},
    store::agents::AgentStore,
    store::sessions::SessionReader,
    store::skills::SkillStore,
};
use trogon_xai_runner::{
    AgentLoader, SessionNotifier, SkillLoader, XaiClient,
    XaiAgent,
    session_store::{NatsSessionStore, SessionStoring},
};

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn make_js() -> (async_nats::jetstream::Context, impl Drop) {
    let container = Nats::default()
        .with_cmd(["--jetstream"])
        .start()
        .await
        .expect("start NATS container");
    let port = container.get_host_port_ipv4(4222).await.expect("port");
    let nats = async_nats::connect(format!("nats://127.0.0.1:{port}"))
        .await
        .expect("connect to NATS");
    (async_nats::jetstream::new(nats), container)
}

// ── No-op notifier (ACP client not needed for this test) ─────────────────────

struct NoOpNotifier;

#[async_trait(?Send)]
impl SessionNotifier for NoOpNotifier {
    async fn notify(&self, _: SessionNotification) {}
}

// ── Test ──────────────────────────────────────────────────────────────────────

/// Full round-trip:
///   trogon-console writes agent + skill to NATS KV
///   → XaiAgent reads config, sends real prompt to xAI API
///   → XaiAgent writes session snapshot to SESSIONS bucket
///   → trogon-console SessionReader reads back the session
///   → assertions on agent_id, token counts, message count, stop reason
#[tokio::test]
async fn xai_agent_console_end_to_end() {
    let api_key = match std::env::var("XAI_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            eprintln!("XAI_API_KEY not set — skipping console e2e test");
            return;
        }
    };

    let (js, _container) = make_js().await;

    // ── 1. Seed trogon-console data ──────────────────────────────────────────

    let agent_store = AgentStore::open(&js).await.expect("AgentStore::open");
    let skill_store = SkillStore::open(&js).await.expect("SkillStore::open");
    let session_reader = SessionReader::open(&js).await.expect("SessionReader::open");

    let skill_id = "concise-helper";
    let agent_id = "e2e-agent-001";
    let now = "1745000000".to_string();
    let skill_version = "20260421".to_string();

    skill_store
        .put(&Skill {
            id: skill_id.to_string(),
            name: "Concise Helper".to_string(),
            description: "Keeps answers short".to_string(),
            provider: "custom".to_string(),
            latest_version: skill_version.clone(),
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .await
        .expect("put skill");

    skill_store
        .put_version(&SkillVersion {
            skill_id: skill_id.to_string(),
            version: skill_version.clone(),
            content: "Always reply with the shortest possible answer. One sentence maximum."
                .to_string(),
            is_latest: true,
            created_at: now.clone(),
        })
        .await
        .expect("put skill version");

    agent_store
        .put(&AgentDefinition {
            id: agent_id.to_string(),
            name: "E2E Test Agent".to_string(),
            description: "Used in console e2e integration test".to_string(),
            status: AgentStatus::Active,
            version: 1,
            model: AgentModel {
                id: "grok-3-mini".to_string(),
                speed: "standard".to_string(),
            },
            system_prompt: "You are a helpful assistant.".to_string(),
            skill_ids: vec![skill_id.to_string()],
            tools: vec![],
            mcp_servers: vec![],
            metadata: serde_json::Value::Null,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .await
        .expect("put agent");

    // ── 2. Build XaiAgent with real loaders + session store ──────────────────

    let agent_loader = AgentLoader::open(&js).await.expect("AgentLoader::open");
    let skill_loader = SkillLoader::open(&js).await.expect("SkillLoader::open");
    let session_store = NatsSessionStore::open(&js).await.expect("NatsSessionStore::open");

    let agent = XaiAgent::with_deps(NoOpNotifier, "grok-3-mini", api_key, XaiClient::new())
        .with_loaders(agent_id, Arc::new(agent_loader), Arc::new(skill_loader))
        .with_session_store(Arc::new(session_store) as Arc<dyn SessionStoring>);

    // ── 3. Run a real prompt ─────────────────────────────────────────────────

    // XaiAgent uses async_trait(?Send) — must run inside a LocalSet.
    let local = tokio::task::LocalSet::new();
    let (session_id, stop_reason) = local
        .run_until(async move {
            let resp = agent
                .new_session(NewSessionRequest::new("/tmp"))
                .await
                .expect("new_session");
            let session_id = resp.session_id.to_string();

            let prompt_resp = agent
                .prompt(PromptRequest::new(
                    session_id.clone(),
                    vec![ContentBlock::from("What is 2+2?")],
                ))
                .await
                .expect("prompt");

            (session_id, prompt_resp.stop_reason)
        })
        .await;

    // ── 4. Verify via trogon-console's SessionReader ─────────────────────────

    // Small delay to ensure the NATS write has propagated.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let console_session = session_reader
        .get("default", &session_id)
        .await
        .expect("SessionReader::get")
        .expect("session must be visible in the SESSIONS bucket");

    assert_eq!(
        stop_reason,
        StopReason::EndTurn,
        "prompt must complete normally, not be cancelled or time out"
    );
    assert_eq!(
        console_session.agent_id.as_deref(),
        Some(agent_id),
        "agent_id must be written to the session snapshot by build_snapshot()"
    );
    assert!(
        console_session.message_count >= 2,
        "must have at least user + assistant message, got {}",
        console_session.message_count
    );
    assert!(
        console_session.output_tokens > 0,
        "output_tokens must be > 0 after a real xAI prompt"
    );
    assert_eq!(
        console_session.tenant_id, "default",
        "tenant_id must default to 'default'"
    );
    assert!(
        !console_session.name.is_empty(),
        "session name must be derived from the first user message"
    );
}
