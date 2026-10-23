use super::*;
use crate::session::tests::make_session_and_context_with_rx;
use codex_protocol::AgentPath;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::turn_input::TurnStartOptions;
use pretty_assertions::assert_eq;

struct WaitingTask;
impl SessionTask for WaitingTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Compact
    }
    fn span_name(&self) -> &'static str {
        "test.cache_deadline_admission"
    }
    async fn run(
        self: Arc<Self>,
        _session: Arc<Session>,
        _context: Arc<TurnContext>,
        _input: Vec<super::super::TurnInput>,
        cancellation: CancellationToken,
    ) -> SessionTaskResult {
        cancellation.cancelled().await;
        Ok(None)
    }
}

#[tokio::test]
async fn cache_deadline_late_cancelled_response_cannot_rearm_or_clear_its_successor() {
    let (session, old, _events) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            codex_login::CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            Vec::new(),
            |config| {
                config.model = Some("gpt-6-astra".into());
                config.config_layer_stack = config
                    .config_layer_stack
                    .with_user_config(
                        &config.codex_home.join("config.toml"),
                        toml::from_str("[cache_deadline_compact_seconds]\ngpt-6-astra = 3300")
                            .unwrap(),
                    )
                    .unwrap();
            },
        )
        .await;
    session
        .start_task(Arc::clone(&old), Vec::new(), WaitingTask)
        .await;
    let old_step = session
        .capture_step_context(old, &CancellationToken::new())
        .await
        .unwrap();
    let owner = owner_generation(&session);
    record_foreground(&session, &old_step, owner).await;
    assert!(session.cache_deadline.deadline.lock().unwrap().is_some());
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
    // The old request finishes after the abort has detached its turn and cleared the ticket.
    record_foreground(&session, &old_step, owner).await;
    assert!(session.cache_deadline.deadline.lock().unwrap().is_none());
    let next = session.new_default_turn().await;
    session
        .start_task(Arc::clone(&next), Vec::new(), WaitingTask)
        .await;
    let next_step = session
        .capture_step_context(next, &CancellationToken::new())
        .await
        .unwrap();
    record_foreground(&session, &next_step, owner).await;
    let next_due = session
        .cache_deadline
        .deadline
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .due;
    // An old account's late completion must not clear the new turn's valid ticket either.
    record_foreground(&session, &old_step, owner.wrapping_add(1)).await;
    assert_eq!(
        session
            .cache_deadline
            .deadline
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .due,
        next_due
    );
    session.abort_all_tasks(TurnAbortReason::Replaced).await;
}

#[tokio::test]
async fn cache_deadline_discards_suspend_expiry_even_when_monotonic_clock_stopped() {
    let timer = CacheDeadline::default();
    *timer.deadline.lock().unwrap() = Some(Deadline {
        due: Instant::now(),
        expires: Instant::now() + Duration::from_secs(300),
        wall_expires: SystemTime::now() - Duration::from_secs(1),
        wake_pending: true,
        model: "gpt-6-astra".into(),
        owner_generation: 0,
        window: "before-suspend".into(),
        interval: Duration::from_secs(3300),
    });
    assert!(!timer.due());
    assert!(timer.deadline.lock().unwrap().is_none());
}

#[tokio::test]
async fn cache_deadline_task_admission_preserves_racing_mail_and_its_start_options() {
    let (session, context, _events) = make_session_and_context_with_rx().await;
    context.extension_data.insert(CacheDeadlineTask);
    let mail = InterAgentCommunication::new(
        AgentPath::try_from("/root/worker").unwrap(),
        AgentPath::root(),
        Vec::new(),
        "continue after maintenance".into(),
        /*trigger_turn*/ true,
    );
    let options = TurnStartOptions {
        parent_turn_id: Some("parent".into()),
        root_turn_id: Some("root".into()),
        ..Default::default()
    };
    // Reproduce mail arriving after the idle check but before native task admission.
    session
        .input_queue
        .enqueue_mailbox_communication(mail.clone().into(), options.clone())
        .await;
    session.start_task(context, Vec::new(), WaitingTask).await;
    let (pending, retained_options) = session.input_queue.drain_mailbox_input_items().await;
    assert_eq!(
        (
            pending,
            retained_options.parent_turn_id,
            retained_options.root_turn_id
        ),
        (
            vec![super::super::TurnInput::InterAgentCommunication(
                mail.into()
            )],
            options.parent_turn_id,
            options.root_turn_id,
        )
    );
    session.abort_all_tasks(TurnAbortReason::Replaced).await;
}
