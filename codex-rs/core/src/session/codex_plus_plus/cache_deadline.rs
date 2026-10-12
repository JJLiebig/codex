//! One cache deadline per loaded root session, armed only by foreground inference.

use super::session::Session;
use super::step_context::StepContext;
use super::turn_context::TurnContext;
use crate::client::ModelClientSession;
use crate::state::ActiveTurn;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskResult;
use async_channel::Receiver;
use codex_analytics::CompactionPhase;
use codex_analytics::CompactionReason;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TurnAbortReason;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::SystemTime;
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

// Astra/Sol's tested 60-minute lifetime leaves five minutes after the 55-minute default.
const EXPIRY_MARGIN: Duration = Duration::from_secs(5 * 60);

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
        *self.deadline.lock().unwrap() = None;
        self.changed.notify_one();
    }

    pub(super) async fn wait(&self) {
        let notified = self.changed.notified();
        let due = self
            .deadline
            .lock()
            .unwrap()
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
        let mut guard = self.deadline.lock().unwrap();
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
    if matches!(step.turn.session_source, SessionSource::Exec)
        || step.turn.session_source.is_non_root_agent()
    {
        return;
    }
    // A concurrent account switch cannot label the previous account's request as fresh.
    if request_owner != owner_generation(session) {
        session.cache_deadline.clear();
        return;
    }
    let config = session.get_config().await;
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
    let Some(expires) = due.checked_add(EXPIRY_MARGIN) else {
        return;
    };
    let Some(wall_expires) = SystemTime::now()
        .checked_add(interval)
        .and_then(|due| due.checked_add(EXPIRY_MARGIN))
    else {
        return;
    };
    let window = session.current_window_id().await;
    *session.cache_deadline.deadline.lock().unwrap() = Some(Deadline {
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

async fn take_due(session: &Session, model: &str) -> bool {
    if !session.cache_deadline.due() {
        return false;
    }
    let config = session.get_config().await;
    let interval = codex_config::cache_deadline_compact_interval(&config.config_layer_stack, model);
    let window = session.current_window_id().await;
    let owner_generation = owner_generation(session);
    let mut guard = session.cache_deadline.deadline.lock().unwrap();
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
    })
}

pub(super) async fn at_sampling_boundary(
    session: &Arc<Session>,
    step: &Arc<StepContext>,
    client: &mut ModelClientSession,
    attempts: &mut HashSet<String>,
) -> CodexResult<()> {
    if take_due(session, &step.settings.model_info.slug).await {
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
            if matches!(error.details(), CodexErrorDetails::TurnAborted) {
                return Err(error);
            }
            tracing::warn!(%error, "cache deadline compaction failed; continuing foreground turn");
        }
    }
    Ok(())
}

// This future remains inside the submission select, so foreground input cancels slow discovery.
pub(super) async fn prepare_idle(session: &Arc<Session>) -> Option<Arc<TurnContext>> {
    session.cache_deadline.wait().await;
    if !session.cache_deadline.due() {
        return None;
    }
    if session.active_turn.lock().await.is_some() {
        if let Some(deadline) = session.cache_deadline.deadline.lock().unwrap().as_mut()
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
    if !take_due(session, &context.model_info().slug).await {
        session.clear_reserved_idle_turn(&reservation).await;
        return;
    }
    context.extension_data.insert(CacheDeadlineTask);
    session
        .start_task(context, Vec::new(), CacheDeadlineTask)
        .await;
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
        let step = session.capture_step_context(context, &cancellation).await?;
        super::turn::run_auto_compact(
            &session,
            Arc::clone(&step),
            step,
            &mut session.services.model_client.new_session(),
            &mut HashSet::new(),
            CompactionReason::CacheDeadline,
            CompactionPhase::StandaloneTurn,
        )
        .await?;
        Ok(None)
    }
}
