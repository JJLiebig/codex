use std::time::Duration;

use codex_config::ModelCapacityRetryMode;
use codex_core::TurnInputRequest;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_failed;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use wiremock::ResponseTemplate;

const BOUNDED_CAPACITY_MESSAGES: [&str; 4] = [
    "The selected model is at capacity. Retrying in 1 minute (1/4).",
    "The selected model is at capacity. Retrying in 2 minutes (2/4).",
    "The selected model is at capacity. Retrying in 5 minutes (3/4).",
    "The selected model is at capacity. Retrying in 15 minutes (4/4).",
];
const CAPACITY_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(2 * 60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
];

async fn submit(codex: &codex_core::CodexThread) {
    codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".into(),
            text_elements: Vec::new(),
        }]))
        .await
        .expect("submit turn");
}

#[tokio::test]
async fn capacity_retry_waits_one_minute_and_preserves_input() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse_failed("resp-overloaded", "server_is_overloaded", "at capacity"),
            sse(vec![
                ev_response_created("resp-ok"),
                ev_completed("resp-ok"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
        })
        .build(&server)
        .await?;

    submit(&test.codex).await;
    let warning = wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::Warning(warning) if warning.message == BOUNDED_CAPACITY_MESSAGES[0]),
    )
    .await;
    let EventMsg::Warning(warning) = warning else {
        unreachable!();
    };
    assert_eq!(warning.message, BOUNDED_CAPACITY_MESSAGES[0]);
    assert_eq!(responses.requests().len(), 1);
    tokio::time::pause();
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert_eq!(responses.requests().len(), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::time::resume();
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let requests = responses.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].input(), requests[1].input());
    Ok(())
}

fn upstream_gateway_reset() -> ResponseTemplate {
    ResponseTemplate::new(503).set_body_string(
        "upstream connect error or disconnect/reset before headers. reset reason: remote connection failure",
    )
}

#[tokio::test]
async fn exhausted_stream_retries_back_off_with_same_prompt_and_cache_key() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let test = test_codex()
        .with_session_source(SessionSource::Cli)
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = None;
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex
        .set_app_server_client_info(
            Some("codex-tui".to_string()),
            /*app_server_client_version*/ None,
        )
        .await?;

    let fast_attempts = test.config.model_provider.stream_max_retries() as usize + 1;
    let broken_stream = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .insert_header("content-encoding", "gzip")
        .set_body_bytes(b"invalid gzip body");
    let success = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse(vec![
            ev_response_created("resp-ok"),
            ev_completed("resp-ok"),
        ]));
    let mut responses = vec![upstream_gateway_reset(); fast_attempts - 1];
    responses.extend([broken_stream, upstream_gateway_reset(), success]);
    let responses = mount_response_sequence(&server, responses).await;

    submit(&test.codex).await;
    for (requests_before_retry, delay) in [(fast_attempts, 60), (fast_attempts + 1, 120)] {
        let event = wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::StreamError(error) if error.message == "Reconnecting... waiting for network")
                || matches!(event, EventMsg::Error(_) | EventMsg::TurnComplete(_))
        })
        .await;
        assert!(matches!(event, EventMsg::StreamError(_)), "{event:?}");
        assert_eq!(responses.requests().len(), requests_before_retry);

        tokio::time::pause();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(delay - 1)).await;
        assert_eq!(responses.requests().len(), requests_before_retry);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::time::resume();
    }
    let event = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::Error(_) | EventMsg::TurnComplete(_))
    })
    .await;
    assert!(matches!(event, EventMsg::TurnComplete(_)), "{event:?}");

    let requests = responses.requests();
    assert_eq!(requests.len(), fast_attempts + 2);
    let initial = requests[0].body_json();
    let session_id = requests[0].header("session-id");
    assert!(session_id.is_some());
    assert!(initial["prompt_cache_key"].as_str().is_some());
    for request in &requests[1..] {
        let candidate = request.body_json();
        assert_eq!(candidate["model"], initial["model"]);
        assert_eq!(candidate["input"], initial["input"]);
        assert_eq!(candidate["tools"], initial["tools"]);
        assert_eq!(candidate["prompt_cache_key"], initial["prompt_cache_key"]);
        assert_eq!(request.header("session-id"), session_id);
    }
    Ok(())
}

#[tokio::test]
async fn exhausted_stream_retries_stop_for_explicit_limits_and_noninteractive_calls()
-> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    for (source, client_name, configured_limit) in [
        (SessionSource::Cli, Some("codex-tui"), Some(1)),
        (SessionSource::Cli, Some("codex_exec"), None),
        (
            SessionSource::Custom("thread_sample".to_string()),
            None,
            None,
        ),
    ] {
        let server = start_mock_server().await;
        let test = test_codex()
            .with_session_source(source)
            .with_config(move |config| {
                config.model_provider.request_max_retries = Some(0);
                config.model_provider.stream_max_retries = configured_limit;
            })
            .build_with_auto_env(&server)
            .await?;
        test.codex
            .set_app_server_client_info(
                client_name.map(str::to_string),
                /*app_server_client_version*/ None,
            )
            .await?;
        let fast_attempts = test.config.model_provider.stream_max_retries() as usize + 1;
        let responses =
            mount_response_sequence(&server, vec![upstream_gateway_reset(); fast_attempts]).await;

        submit(&test.codex).await;
        let event = wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::StreamError(error) if error.message == "Reconnecting... waiting for network")
                || matches!(event, EventMsg::Error(_) | EventMsg::TurnComplete(_))
        })
        .await;
        assert!(
            matches!(event, EventMsg::Error(_)),
            "{client_name:?}, {configured_limit:?}: {event:?}"
        );
        assert_eq!(responses.requests().len(), fast_attempts);
    }
    Ok(())
}

#[tokio::test]
async fn interrupt_cancels_capacity_retry_wait() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let responses = mount_sse_once(
        &server,
        sse_failed("resp-overloaded", "server_is_overloaded", "at capacity"),
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
        })
        .build(&server)
        .await?;

    submit(&test.codex).await;
    wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::Warning(warning) if warning.message == BOUNDED_CAPACITY_MESSAGES[0]),
    )
    .await;
    tokio::time::pause();
    tokio::task::yield_now().await;
    test.codex.submit(Op::Interrupt).await?;
    tokio::time::resume();
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    tokio::time::resume();
    assert_eq!(responses.requests().len(), 1);
    Ok(())
}

#[tokio::test]
async fn exhausted_capacity_retry_budget_surfaces_original_error() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse_failed("resp-overloaded-1", "server_is_overloaded", "at capacity"),
            sse_failed("resp-overloaded-2", "server_is_overloaded", "at capacity"),
            sse_failed("resp-overloaded-3", "server_is_overloaded", "at capacity"),
            sse_failed("resp-overloaded-4", "server_is_overloaded", "at capacity"),
            sse_failed(
                "resp-overloaded-5",
                "server_is_overloaded",
                "still at capacity",
            ),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
        })
        .build(&server)
        .await?;

    submit(&test.codex).await;
    wait_for_event(
        &test.codex,
        |event| matches!(event, EventMsg::Warning(warning) if warning.message == BOUNDED_CAPACITY_MESSAGES[0]),
    )
    .await;
    assert_eq!(responses.requests().len(), 1);
    for index in 1..BOUNDED_CAPACITY_MESSAGES.len() {
        tokio::time::pause();
        tokio::task::yield_now().await;
        tokio::time::advance(CAPACITY_RETRY_DELAYS[index - 1]).await;
        tokio::time::resume();
        wait_for_event(
            &test.codex,
            |event| matches!(event, EventMsg::Warning(warning) if warning.message == BOUNDED_CAPACITY_MESSAGES[index]),
        )
        .await;
        assert_eq!(responses.requests().len(), index + 1);
    }
    tokio::time::pause();
    tokio::task::yield_now().await;
    tokio::time::advance(CAPACITY_RETRY_DELAYS[3]).await;
    tokio::time::resume();
    let error = wait_for_event(&test.codex, |event| matches!(event, EventMsg::Error(_))).await;
    let EventMsg::Error(error) = error else {
        unreachable!();
    };
    assert_eq!(error.message, CodexErr::ServerOverloaded.to_string());
    assert_eq!(
        error.codex_error_info,
        Some(CodexErrorInfo::ServerOverloaded)
    );
    assert_eq!(responses.requests().len(), 5);
    Ok(())
}

#[tokio::test]
async fn indefinite_capacity_retry_continues_at_fifteen_minutes() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    let server = start_mock_server().await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse_failed("resp-overloaded-1", "server_is_overloaded", "at capacity"),
            sse_failed("resp-overloaded-2", "server_is_overloaded", "at capacity"),
            sse_failed("resp-overloaded-3", "server_is_overloaded", "at capacity"),
            sse_failed("resp-overloaded-4", "server_is_overloaded", "at capacity"),
            sse_failed("resp-overloaded-5", "server_is_overloaded", "at capacity"),
            sse(vec![
                ev_response_created("resp-ok"),
                ev_completed("resp-ok"),
            ]),
        ],
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
            config.model_capacity_retry_mode = ModelCapacityRetryMode::Indefinite;
        })
        .build(&server)
        .await?;

    submit(&test.codex).await;
    let first_retry = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::Warning(_) | EventMsg::Error(_))
    })
    .await;
    let EventMsg::Warning(first_retry) = first_retry else {
        panic!("expected capacity warning, got {first_retry:?}");
    };
    assert_eq!(
        first_retry.message,
        "The selected model is at capacity. Retrying in 1 minute (retry 1; indefinite)."
    );
    assert_eq!(responses.requests().len(), 1);
    for retry_count in 2..=5 {
        tokio::time::pause();
        tokio::task::yield_now().await;
        tokio::time::advance(CAPACITY_RETRY_DELAYS[(retry_count - 2).min(3)]).await;
        tokio::time::resume();
        wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::Warning(warning) if warning.message.contains(&format!("retry {retry_count}; indefinite")))
        })
        .await;
        assert_eq!(responses.requests().len(), retry_count);
    }
    tokio::time::pause();
    tokio::task::yield_now().await;
    tokio::time::advance(CAPACITY_RETRY_DELAYS[3]).await;
    tokio::time::resume();
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(responses.requests().len(), 6);
    Ok(())
}
