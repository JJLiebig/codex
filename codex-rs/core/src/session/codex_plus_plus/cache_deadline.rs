//! One cache deadline per loaded root session, armed only by foreground inference.

use super::session::Session;
use super::step_context::StepContext;
use super::turn_context::TurnContext;
use crate::client::ModelClientSession;
use crate::state::ActiveTurn;
use crate::state::MailboxDeliveryPhase;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskResult;
use async_channel::Receiver;
use codex_analytics::CompactionPhase;
use codex_analytics::CompactionReason;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::protocol::ThreadSource;
use codex_protocol::protocol::TurnAbortReason;
use std::collections::HashSet;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::SystemTime;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

// Unknown model lifetimes are not inferred: skip maintenance more than five minutes late.
const MAX_LATENESS: Duration = Duration::from_secs(5 * 60);

struct Deadline {
    due: Instant,
    expires: Instant,
    wall_expires: SystemTime,
    wake_pending: bool,
    model: String,
    owner_generation: u64,
    window: String,
    interval: Duration,
}

#[derive(Default)]
pub(crate) struct CacheDeadline {
    deadline: Mutex<Option<Deadline>>,
    pub(crate) changed: Notify,
}

impl CacheDeadline {
    pub(crate) fn clear(&self) {
        *self
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.changed.notify_one();
    }

    pub(super) async fn wait(&self) {
        let notified = self.changed.notified();
        let due = self
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|deadline| deadline.wake_pending)
            .map(|deadline| deadline.due);
        match due {
            Some(due) => tokio::select! {
                _ = tokio::time::sleep_until(due) => {},
                _ = notified => {},
            },
            None => notified.await,
        }
    }

    fn due(&self) -> bool {
        let mut guard = self
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(deadline) = guard.as_mut() else {
            return false;
        };
        if Instant::now() >= deadline.expires || SystemTime::now() >= deadline.wall_expires {
            *guard = None;
            return false;
        }
        if Instant::now() < deadline.due {
            return false;
        }
        true
    }
}

pub(super) fn owner_generation(session: &Session) -> u64 {
    session
        .services
        .auth_manager
        .auth_change_state_receiver()
        .borrow()
        .owner_generation
}

pub(super) async fn record_foreground(session: &Session, step: &StepContext, request_owner: u64) {
    if step.turn.session_source.is_non_root_agent() {
        return;
    }
    if step.turn.config.ephemeral
        && matches!(
            session.state.lock().await.session_configuration.thread_source.as_ref(),
            Some(ThreadSource::Feature(feature)) if feature == "thread_title"
        )
    {
        return;
    }
    let config = session.get_config().await;
    let window = session.current_window_id().await;
    // Serialize ticket mutation with abort removing its owner, including late response completion.
    let active = session.active_turn.lock().await;
    if !active
        .as_ref()
        .and_then(|turn| turn.task.as_ref())
        .is_some_and(|task| task.turn_context.sub_id == step.turn.sub_id)
    {
        return;
    }
    // A concurrent account switch cannot label the previous account's request as fresh.
    if request_owner != owner_generation(session) {
        session.cache_deadline.clear();
        return;
    }
    let Some(interval) = codex_config::cache_deadline_compact_interval(
        &config.config_layer_stack,
        &step.settings.model_info.slug,
    ) else {
        session.cache_deadline.clear();
        return;
    };
    let now = Instant::now();
    let Some(due) = now.checked_add(interval) else {
        return;
    };
    let Some(fresh_for) =
        codex_config::cache_deadline_known_lifetime(&step.settings.model_info.slug)
            .or_else(|| interval.checked_add(MAX_LATENESS))
    else {
        return;
    };
    let Some(expires) = now.checked_add(fresh_for) else {
        return;
    };
    let Some(wall_expires) = SystemTime::now().checked_add(fresh_for) else {
        return;
    };
    *session
        .cache_deadline
        .deadline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Deadline {
        due,
        expires,
        wall_expires,
        wake_pending: true,
        model: step.settings.model_info.slug.clone(),
        owner_generation: request_owner,
        window,
        interval,
    });
    session.cache_deadline.changed.notify_one();
}

async fn take_due(session: &Session, model_info: &ModelInfo) -> bool {
    if !session.cache_deadline.due() {
        return false;
    }
    let config = session.get_config().await;
    let model = model_info.slug.as_str();
    let interval = codex_config::cache_deadline_compact_interval(&config.config_layer_stack, model);
    let window = session.current_window_id().await;
    let owner_generation = owner_generation(session);
    let context_tokens = session.get_total_token_usage().await;
    let mut guard = session
        .cache_deadline
        .deadline
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard
        .as_ref()
        .is_some_and(|deadline| Instant::now() < deadline.due)
    {
        return false;
    }
    let deadline = guard.take();
    deadline.is_some_and(|deadline| {
        deadline.model == model
            && deadline.window == window
            && deadline.owner_generation == owner_generation
            && Some(deadline.interval) == interval
            && Instant::now() < deadline.expires
            && SystemTime::now() < deadline.wall_expires
            // Consume small-context deadlines too; only foreground inference rearms maintenance.
            && model_info.usable_context_window().is_some_and(|limit| {
                limit > 0 && i128::from(context_tokens) * 100 >= i128::from(limit) * 25
            })
    })
}

pub(super) async fn at_sampling_boundary(
    session: &Arc<Session>,
    step: &Arc<StepContext>,
    client: &mut ModelClientSession,
    attempts: &mut HashSet<String>,
) -> CodexResult<ControlFlow<()>> {
    if take_due(session, &step.settings.model_info).await {
        let result = super::turn::run_auto_compact(
            session,
            Arc::clone(step),
            Arc::clone(step),
            client,
            attempts,
            CompactionReason::CacheDeadline,
            CompactionPhase::MidTurn,
        )
        .await;
        if let Err(error) = result {
            report_failure(session, &step.turn, error).await?;
            return Ok(ControlFlow::Break(()));
        }
    }
    Ok(ControlFlow::Continue(()))
}

// This future remains inside the submission select, so foreground input cancels slow discovery.
pub(super) async fn prepare_idle(session: &Arc<Session>) -> Option<Arc<TurnContext>> {
    session.cache_deadline.wait().await;
    if !session.cache_deadline.due() {
        return None;
    }
    if session.active_turn.lock().await.is_some() {
        if let Some(deadline) = session
            .cache_deadline
            .deadline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
            && deadline.due <= Instant::now()
        {
            deadline.wake_pending = false;
        }
        return None;
    }
    Some(session.new_default_turn().await)
}

pub(super) async fn start_if_idle(
    session: &Arc<Session>,
    submissions: &Receiver<super::Submission>,
    context: Arc<TurnContext>,
) {
    let Some(_admission) = session.services.extensions.admit_turn_start() else {
        session.cache_deadline.clear();
        return;
    };
    if session.input_queue.has_pending_mailbox_items().await {
        session.cache_deadline.clear();
        return;
    }
    if !submissions.is_empty()
        || submissions.is_closed()
        || session.services.local_agent_runtime.shutdown.is_cancelled()
    {
        return;
    }
    let reservation = {
        let mut active = session.active_turn.lock().await;
        if active.is_some() {
            return;
        }
        let active = active.get_or_insert_with(ActiveTurn::default);
        Arc::clone(&active.turn_state)
    };
    if !take_due(session, &context.model_info()).await {
        session.clear_reserved_idle_turn(&reservation).await;
        return;
    }
    reservation
        .lock()
        .await
        .set_mailbox_delivery_phase(MailboxDeliveryPhase::NextTurn);
    context.extension_data.insert(CacheDeadlineTask);
    session
        .start_task(context, Vec::new(), CacheDeadlineTask)
        .await;
}

// Mail arriving during admission belongs to a regular turn, never to optional maintenance.
pub(crate) async fn starting_mailbox_input(
    session: &Session,
    context: &TurnContext,
) -> Vec<super::TurnInput> {
    if context.extension_data.get::<CacheDeadlineTask>().is_some() {
        Vec::new()
    } else {
        session.input_queue.drain_mailbox_input_items().await.0
    }
}

/// Foreground submissions can cancel this optional maintenance through normal task cancellation.
pub(super) async fn yield_to_foreground(session: &Arc<Session>) {
    let maintenance = session
        .active_turn
        .lock()
        .await
        .as_ref()
        .and_then(|active| active.task.as_ref())
        .filter(|task| {
            task.turn_context
                .extension_data
                .get::<CacheDeadlineTask>()
                .is_some()
        })
        .map(|task| task.turn_context.sub_id.clone());
    if let Some(id) = maintenance {
        session
            .abort_turn_if_active(&id, TurnAbortReason::Replaced, /*error*/ None)
            .await;
    }
}

#[derive(Clone)]
struct CacheDeadlineTask;

impl SessionTask for CacheDeadlineTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Compact
    }
    fn span_name(&self) -> &'static str {
        "session_task.cache_deadline_compact"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<Session>,
        context: Arc<TurnContext>,
        _input: Vec<super::TurnInput>,
        cancellation: CancellationToken,
    ) -> SessionTaskResult {
        session.emit_turn_started(&context, TaskKind::Compact).await;
        if session.input_queue.has_pending_mailbox_items().await {
            return Ok(None);
        }
        let step = session
            .capture_step_context(Arc::clone(&context), &cancellation)
            .await?;
        let result = super::turn::run_auto_compact(
            &session,
            Arc::clone(&step),
            step,
            &mut session.services.model_client.new_session(),
            &mut HashSet::new(),
            CompactionReason::CacheDeadline,
            CompactionPhase::StandaloneTurn,
        )
        .await;
        if let Err(error) = result {
            report_failure(&session, &context, error).await?;
        }
        Ok(None)
    }
}

async fn report_failure(
    session: &Session,
    context: &TurnContext,
    error: CodexErr,
) -> CodexResult<()> {
    if matches!(error.details(), CodexErrorDetails::TurnAborted)
        || context.terminal_error.lock().await.is_none()
    {
        return Err(error);
    }
    // Native compaction already reported this terminal failure; end work without reporting twice.
    session
        .emit_turn_error_lifecycle(context, error.to_codex_protocol_error(), error.details())
        .await;
    tracing::warn!(%error, "cache deadline compaction failed");
    Ok(())
}

#[cfg(test)]
#[path = "cache_deadline_tests.rs"]
mod tests;
