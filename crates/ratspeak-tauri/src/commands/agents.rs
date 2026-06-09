//! Owner-side agent management commands for Settings > Agents.

use std::sync::Arc;

use ratspeak_cli::agent_actions;
use ratspeak_cli::agent_admin::{self, AgentCreateOptions, AgentGrantUpdate, AgentPolicyPatch};
use ratspeak_cli::profile::Profile;
use serde::Deserialize;
use serde_json::{Value, json};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Default, Deserialize)]
pub struct CreateAgentArgs {
    pub name: String,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub presets: Vec<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub allowed_contacts: Vec<String>,
    #[serde(default)]
    pub allowed_conversations: Vec<String>,
    #[serde(default)]
    pub unknown_contacts: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct SetAgentGrantArgs {
    pub name: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(default)]
    pub presets: Vec<String>,
    #[serde(default)]
    pub contacts: Vec<String>,
    #[serde(default)]
    pub conversations: Vec<String>,
    #[serde(default)]
    pub unknown_contacts: Option<String>,
    #[serde(default)]
    pub replace_scopes: bool,
    #[serde(default)]
    pub replace_contacts: bool,
    #[serde(default)]
    pub replace_conversations: bool,
    #[serde(default)]
    pub activate: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentActionArgs {
    #[serde(default)]
    pub agent: Option<String>,
    pub id: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub execute: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentFileInspectArgs {
    #[serde(default)]
    pub agent: Option<String>,
    pub id: String,
    #[serde(default)]
    pub file_id: Option<String>,
    #[serde(default)]
    pub preview_bytes: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentPolicySetArgs {
    pub name: String,
    #[serde(default)]
    pub patch: Option<AgentPolicyPatch>,
    #[serde(default)]
    pub set: Vec<agent_admin::PolicySet>,
    #[serde(default)]
    pub policy: Option<agent_actions::AgentWritePolicy>,
}

fn owner_profile(state: &AppState) -> Profile {
    Profile {
        data_root: state.config.data_root.clone(),
        config: state.config.clone(),
        db: state.db.clone(),
    }
}

fn cli_to_app(error: ratspeak_cli::CliError) -> AppError {
    let code = error.code();
    if code == "usage" {
        AppError::bad_request(error.to_string())
    } else {
        AppError::new(code, error.to_string())
    }
}

async fn run_agent_task<F>(state: State<'_, Arc<AppState>>, task: F) -> AppResult<Value>
where
    F: FnOnce(Profile) -> ratspeak_cli::CliResult<Value> + Send + 'static,
{
    let profile = owner_profile(&state);
    tokio::task::spawn_blocking(move || task(profile))
        .await
        .map_err(|e| AppError::internal(format!("agent admin task panicked: {e}")))?
        .map_err(cli_to_app)
}

fn emit_agents_updated(state: &AppState, payload: &Value) {
    state.emit_to_all(
        "agents_updated",
        json!({
            "agent": payload.get("agent").cloned().unwrap_or(Value::Null),
            "changed": payload.get("changed").cloned().unwrap_or(Value::Bool(true)),
        }),
    );
}

fn emit_agent_actions_updated(state: &AppState, payload: &Value) {
    state.emit_to_all(
        "agent_actions_updated",
        json!({
            "agent": payload.get("agent").cloned().unwrap_or(Value::Null),
            "action_id": payload.get("id").cloned().unwrap_or(Value::Null),
            "state": payload.get("state").cloned().unwrap_or(Value::Null),
        }),
    );
}

#[tauri::command]
pub async fn api_agents(state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    run_agent_task(state, |profile| agent_admin::list_agent_summaries(&profile)).await
}

#[tauri::command]
pub async fn api_agent(state: State<'_, Arc<AppState>>, name: String) -> AppResult<Value> {
    run_agent_task(state, move |profile| {
        agent_admin::show_agent_bundle(&profile, &name)
    })
    .await
}

#[tauri::command]
pub async fn api_agent_connection_bundle(
    state: State<'_, Arc<AppState>>,
    name: String,
) -> AppResult<Value> {
    run_agent_task(state, move |profile| {
        agent_admin::connection_bundle(&profile, &name)
    })
    .await
}

#[tauri::command]
pub async fn create_agent(
    state: State<'_, Arc<AppState>>,
    args: CreateAgentArgs,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        let mut presets = args.presets;
        if let Some(preset) = args.preset {
            presets.push(preset);
        }
        agent_admin::create_agent(
            &profile,
            AgentCreateOptions {
                name: args.name,
                identity_mode: "new".into(),
                explicit_profile_dir: None,
                requested_scopes: args.scopes,
                presets,
                allowed_contacts: args.allowed_contacts,
                allowed_conversations: args.allowed_conversations,
                unknown_contacts: args.unknown_contacts.unwrap_or_else(|| "deny".into()),
                nickname: args.nickname,
            },
        )
    })
    .await?;
    emit_agents_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn set_agent_grant(
    state: State<'_, Arc<AppState>>,
    args: SetAgentGrantArgs,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        let mut presets = args.presets;
        if let Some(preset) = args.preset {
            presets.push(preset);
        }
        agent_admin::update_agent_grant(
            &profile,
            AgentGrantUpdate {
                name: args.name,
                scopes: args.scopes,
                presets,
                contacts: args.contacts,
                conversations: args.conversations,
                unknown_contacts: args.unknown_contacts,
                replace_scopes: args.replace_scopes,
                replace_contacts: args.replace_contacts,
                replace_conversations: args.replace_conversations,
                activate: args.activate,
            },
        )
    })
    .await?;
    emit_agents_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn revoke_agent(
    state: State<'_, Arc<AppState>>,
    name: String,
    reason: Option<String>,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        agent_admin::revoke_agent(&profile, &name, reason)
    })
    .await?;
    emit_agents_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn rotate_agent_token(state: State<'_, Arc<AppState>>, name: String) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        agent_admin::rotate_agent_token(&profile, &name)
    })
    .await?;
    emit_agents_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn api_agent_policy(state: State<'_, Arc<AppState>>, name: String) -> AppResult<Value> {
    run_agent_task(state, move |profile| {
        agent_admin::show_agent_policy(&profile, &name)
    })
    .await
}

#[tauri::command]
pub async fn api_agent_policy_defaults(_state: State<'_, Arc<AppState>>) -> AppResult<Value> {
    Ok(json!({
        "policy": agent_actions::AgentWritePolicy::default(),
        "presets": agent_admin::agent_presets_payload(),
    }))
}

#[tauri::command]
pub async fn set_agent_policy(
    state: State<'_, Arc<AppState>>,
    args: AgentPolicySetArgs,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        let patch = args.patch.unwrap_or(AgentPolicyPatch {
            policy: args.policy,
            set: args.set,
        });
        agent_admin::set_agent_policy(&profile, &args.name, patch)
    })
    .await?;
    emit_agents_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn api_agent_approvals(
    state: State<'_, Arc<AppState>>,
    agent: Option<String>,
    state_filter: Option<String>,
) -> AppResult<Value> {
    run_agent_task(state, move |profile| {
        agent_admin::list_agent_approvals(&profile, agent.as_deref(), state_filter.as_deref())
    })
    .await
}

#[tauri::command]
pub async fn api_agent_approval(
    state: State<'_, Arc<AppState>>,
    agent: Option<String>,
    id: String,
) -> AppResult<Value> {
    run_agent_task(state, move |profile| {
        agent_admin::show_agent_approval(&profile, agent.as_deref(), &id)
    })
    .await
}

#[tauri::command]
pub async fn api_agent_file_inspection(
    state: State<'_, Arc<AppState>>,
    args: AgentFileInspectArgs,
) -> AppResult<Value> {
    run_agent_task(state, move |profile| {
        agent_admin::inspect_agent_staged_file(
            &profile,
            args.agent.as_deref(),
            &args.id,
            args.file_id.as_deref(),
            args.preview_bytes.unwrap_or(1000),
        )
    })
    .await
}

#[tauri::command]
pub async fn approve_agent_action(
    state: State<'_, Arc<AppState>>,
    args: AgentActionArgs,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        agent_admin::approve_agent_action(
            &profile,
            args.agent.as_deref(),
            &args.id,
            args.note,
            args.execute,
        )
    })
    .await?;
    emit_agent_actions_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn reject_agent_action(
    state: State<'_, Arc<AppState>>,
    args: AgentActionArgs,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        agent_admin::reject_agent_action(&profile, args.agent.as_deref(), &args.id, args.note)
    })
    .await?;
    emit_agent_actions_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn cancel_agent_action(
    state: State<'_, Arc<AppState>>,
    args: AgentActionArgs,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        agent_admin::cancel_agent_action(&profile, args.agent.as_deref(), &args.id, args.note)
    })
    .await?;
    emit_agent_actions_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn execute_agent_action(
    state: State<'_, Arc<AppState>>,
    args: AgentActionArgs,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        agent_admin::execute_agent_action(&profile, args.agent.as_deref(), &args.id)
    })
    .await?;
    emit_agent_actions_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn expire_agent_actions(
    state: State<'_, Arc<AppState>>,
    agent: Option<String>,
) -> AppResult<Value> {
    let event_state = state.inner().clone();
    let payload = run_agent_task(state, move |profile| {
        agent_admin::expire_agent_actions(&profile, agent.as_deref())
    })
    .await?;
    emit_agent_actions_updated(&event_state, &payload);
    Ok(payload)
}

#[tauri::command]
pub async fn api_agent_audit(
    state: State<'_, Arc<AppState>>,
    agent: Option<String>,
    limit: Option<usize>,
) -> AppResult<Value> {
    run_agent_task(state, move |profile| {
        agent_admin::list_agent_audit(&profile, agent.as_deref(), limit.unwrap_or(100))
    })
    .await
}
