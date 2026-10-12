use super::*;
use crate::session::tests::make_session_and_context_with_rx;
use codex_protocol::AgentPath;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::turn_input::TurnStartOptions;
use pretty_assertions::assert_eq;

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
        .enqueue_mailbox_communication(mail.clone(), options.clone())
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
            vec![super::super::TurnInput::InterAgentCommunication(mail)],
            options.parent_turn_id,
            options.root_turn_id,
        )
    );
    session.abort_all_tasks(TurnAbortReason::Replaced).await;
}
