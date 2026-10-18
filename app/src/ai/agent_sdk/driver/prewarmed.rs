use std::pin::Pin;

use warp_cli::skill::SkillSpec;
use warp_graphql::queries::execution_config::ExecutionConfiguration;

use super::*;
use crate::ai::agent_sdk::driver::agent_setup::configured_skills::resolve_configured_skills;
use crate::ai::agent_sdk::{
    AgentDriverRunner, RepositoryContract, build_execution_task_and_options,
};

fn validate_assignment(config: &ExecutionConfiguration) -> anyhow::Result<()> {
    anyhow::ensure!(
        execution_config::harness(&config.harness)? == Harness::Oz,
        "Pre-warmed assignment requires the Oz harness"
    );
    anyhow::ensure!(
        config.conversation_id.is_none() && !config.skip_initial_turn,
        "Pre-warmed assignment does not support resumed or promptless runs"
    );
    anyhow::ensure!(
        FeatureFlag::FactoryDeferredRepositories.is_enabled()
            || config
                .deferred_repositories
                .as_ref()
                .is_none_or(Vec::is_empty),
        "Pre-warmed assignment has deferred repositories this client cannot prepare"
    );
    Ok(())
}

fn assignment_uses_factory_repositories(runtime_env: &HashMap<String, String>) -> bool {
    FeatureFlag::FactoryDeferredRepositories.is_enabled()
        && runtime_env
            .get("WARP_FACTORY_ID")
            .is_some_and(|id| !id.is_empty())
}

fn into_string_env(
    env: HashMap<OsString, OsString>,
    kind: &str,
) -> anyhow::Result<HashMap<String, String>> {
    env.into_iter()
        .map(|(name, value)| {
            Ok((
                name.into_string()
                    .map_err(|_| anyhow!("Invalid {kind} environment name"))?,
                value
                    .into_string()
                    .map_err(|_| anyhow!("Invalid {kind} environment value"))?,
            ))
        })
        .collect()
}

impl AgentDriver {
    /// Restarts the agent's shell so it inherits the environment resolved for the assignment.
    pub async fn replace_shell_with_resolved_env(
        spawner: &ModelSpawner<Self>,
    ) -> Result<(), AgentDriverError> {
        spawner
            .spawn(move |me, ctx| {
                let working_dir = me.harness_working_dir.clone();
                let env_vars = HashMap::clone(&me.resolved_env_vars);
                me.terminal_driver.update(ctx, |driver, ctx| {
                    driver.replace_shell(working_dir, env_vars, ctx)
                })
            })
            .await?
            .await
            .map_err(|error| AgentDriverError::BootstrapFailed { error })
    }

    /// Installs a committed assignment on this standby driver and returns the task to run.
    ///
    /// The server commits the run before delivering it, so the sandbox cannot hand it back and
    /// every failure here must be reported against the run. That report needs the run's own
    /// credential, so the credential is installed before anything else can fail.
    pub(crate) async fn prepare_prewarmed_assignment(
        spawner: &ModelSpawner<Self>,
        config: ExecutionConfiguration,
        runtime_env: HashMap<String, String>,
        api_key: String,
        expires_at: DateTime<Utc>,
    ) -> Result<Task, AgentDriverError> {
        let task_id = config
            .task_id
            .inner()
            .parse::<AmbientAgentTaskId>()
            .map_err(|error| AgentDriverError::ConfigBuildFailed(anyhow!(error)))?;
        let run_key = api_key.clone();
        let ai_client = spawner
            .spawn(move |_, ctx| {
                AuthStateProvider::as_ref(ctx)
                    .get()
                    .set_credentials(Some(Credentials::ApiKey {
                        key: run_key,
                        owner_type: None,
                    }));
                ServerApiProvider::as_ref(ctx).get_ai_client().clone()
            })
            .await?;
        AgentDriverRunner::set_ambient_agent_task_id(spawner, Some(task_id)).await?;
        let result = Self::install_assignment(
            spawner,
            task_id,
            config,
            runtime_env,
            api_key,
            expires_at,
            &ai_client,
        )
        .await;
        if let Err(error) = &result {
            report_driver_error(task_id, error, &ai_client).await;
        }
        result
    }

    async fn install_assignment(
        spawner: &ModelSpawner<Self>,
        task_id: AmbientAgentTaskId,
        mut config: ExecutionConfiguration,
        mut runtime_env: HashMap<String, String>,
        api_key: String,
        expires_at: DateTime<Utc>,
        ai_client: &Arc<dyn AIClient>,
    ) -> Result<Task, AgentDriverError> {
        validate_assignment(&config).map_err(AgentDriverError::ConfigBuildFailed)?;
        let team_scope = execution_config::team_scope(config.team_id.as_ref())
            .map_err(AgentDriverError::ConfigBuildFailed)?;
        let share_requests =
            execution_config::sharing_acls(std::mem::take(&mut config.session_sharing_acls))
                .map_err(AgentDriverError::ConfigBuildFailed)?;
        let bedrock = config
            .inference_providers
            .take()
            .and_then(|providers| providers.aws_bedrock);
        let specs = config
            .skills
            .iter()
            .map(|skill| {
                skill
                    .spec
                    .parse::<SkillSpec>()
                    .map_err(AgentDriverError::SkillResolutionFailed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        runtime_env.insert("WARP_API_KEY".into(), api_key);
        runtime_env.insert(
            "WARP_SANDBOX_DEADLINE".into(),
            expires_at.timestamp().to_string(),
        );
        let use_factory_repositories = assignment_uses_factory_repositories(&runtime_env);
        let reporter_client = ai_client.clone();
        let (server_api, working_dir, setup_events) = spawner
            .spawn(move |me, ctx| {
                let reporter = SetupClientEventReporter::new(
                    task_id,
                    reporter_client,
                    ctx.background_executor(),
                );
                (
                    ServerApiProvider::as_ref(ctx).get(),
                    me.working_dir.clone(),
                    reporter,
                )
            })
            .await?;
        // `gh` is never available to configure github.com inside an assignment sandbox, so the
        // server fetch is the only credential source here.
        AgentDriverRunner::bootstrap_git_credentials_for_task(
            spawner,
            &task_id.to_string(),
            false,
            use_factory_repositories,
        )
        .await?;
        spawner
            .spawn(|_, ctx| {
                Box::pin(crate::ai::agent_sdk::common::refresh_workspace_metadata(
                    ctx,
                )) as Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>
            })
            .await?
            .await
            .map_err(AgentDriverError::TeamMetadataRefreshFailed)?;
        let (first_skill, discovery_dirs) =
            resolve_configured_skills(spawner, specs, false, &working_dir, &setup_events).await?;
        let (mut options, mut task, _, providers) = spawner
            .spawn(move |me, ctx| {
                let result = build_execution_task_and_options(
                    None,
                    config,
                    RepositoryContract {
                        supports_deferred_repositories: FeatureFlag::FactoryDeferredRepositories
                            .is_enabled(),
                        use_factory_repositories,
                    },
                    working_dir,
                    first_skill,
                    discovery_dirs,
                    ctx,
                )?;
                let Some(WorkspaceState::Prepared(prepared)) = me.workspace.as_ref() else {
                    anyhow::bail!("Standby resources were not prepared");
                };
                anyhow::ensure!(
                    prepared.configuration.same_preparation(&result.0.workspace),
                    "Assignment workspace does not match prepared resources"
                );
                Ok::<_, anyhow::Error>(result)
            })
            .await?
            .map_err(AgentDriverError::ConfigBuildFailed)?;
        if FeatureFlag::OzIdentityFederation.is_enabled() {
            options.cloud_providers =
                cloud_provider::load_providers(&providers, &task_id.to_string())
                    .map_err(AgentDriverError::CloudProviderSetupFailed)?;
        }
        options.team_scope = Some(team_scope);
        // The sandbox's runtime environment is the terminal environment for this assignment, so
        // the shared resolver layers managed secrets, task variables, and harness variables
        // underneath it exactly as it does for a normal run.
        options.environment_variables = runtime_env;
        let task_secrets = spawner
            .spawn(move |_, ctx| {
                ManagedSecretManager::as_ref(ctx).get_task_secrets(task_id.to_string())
            })
            .await?
            .await
            .map_err(AgentDriverError::SecretsFetchFailed)?;
        let attachments = if FeatureFlag::AmbientAgentsImageUpload.is_enabled() {
            ai_client.get_task_attachments(task_id.to_string()).await
        } else {
            Ok(Vec::new())
        };
        AgentDriverRunner::prepare_execution_task_data(
            server_api,
            ai_client,
            task_id,
            task_secrets,
            attachments,
            &mut options,
            &mut task,
        )
        .await?;
        #[cfg(not(target_family = "wasm"))]
        if let Some(bedrock) = bedrock {
            let region = bedrock.region.ok_or_else(|| {
                AgentDriverError::AwsBedrockCredentialsFailed("Bedrock region is required".into())
            })?;
            crate::ai::agent_sdk::configure_bedrock_oidc_credentials(
                spawner,
                &mut options,
                bedrock.role_arn,
                region,
            )
            .await?;
        }
        #[cfg(target_family = "wasm")]
        if bedrock.is_some() {
            return Err(AgentDriverError::InvalidRuntimeState);
        }
        let (terminal_inputs, run_config) = split_run_options(options)?;
        let should_share = terminal_inputs.should_share;
        let assignment_env =
            into_string_env(HashMap::clone(&run_config.resolved_env_vars), "assignment")
                .map_err(AgentDriverError::ConfigBuildFailed)?;
        spawner
            .spawn(move |me, ctx| {
                me.apply_run_configuration(run_config, ctx);
                me.sandbox_deadline = Some(expires_at.into());
            })
            .await?;
        Self::replace_shell_with_resolved_env(spawner).await?;
        Self::replace_prewarmed_mcp_environment(spawner, assignment_env).await?;
        spawner
            .spawn(move |me, ctx| {
                let scope = me
                    .team_scope
                    .as_ref()
                    .ok_or(AgentDriverError::InvalidRuntimeState)?;
                me.terminal_driver.update(ctx, |driver, ctx| {
                    driver.bind_prewarmed_task(
                        task_id,
                        me.working_dir.clone(),
                        scope,
                        share_requests,
                        should_share,
                        ctx,
                    )
                })
            })
            .await??;
        spawner
            .spawn(|me, ctx| {
                me.terminal_driver
                    .update(ctx, |driver, _| driver.wait_for_session_shared())
            })
            .await?
            .await?;
        Ok(task)
    }

    pub(crate) async fn run_prewarmed_assignment(
        spawner: &ModelSpawner<Self>,
        task: Task,
    ) -> Result<(), AgentDriverError> {
        spawner.spawn(move |me, ctx| me.run(task, ctx)).await?.await
    }
}

#[cfg(test)]
#[path = "prewarmed_tests.rs"]
mod tests;
