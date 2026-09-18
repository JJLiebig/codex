use super::*;
use codex_app_server_protocol::ThreadStatus;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn completion_wait_flag_tracks_runtime_and_clears_on_terminal_paths() {
    let manager = ThreadWatchManager::new();
    let thread_id = "completion-wait";
    manager.note_turn_started(thread_id).await;
    manager
        .note_background_completion_waiting(thread_id, /*waiting*/ true)
        .await;
    assert_eq!(
        manager.loaded_status_for_thread(thread_id).await,
        ThreadStatus::Active {
            active_flags: vec![ThreadActiveFlag::WaitingOnBackgroundCompletion],
        }
    );
    manager
        .note_background_completion_waiting(thread_id, /*waiting*/ false)
        .await;
    assert_eq!(
        manager.loaded_status_for_thread(thread_id).await,
        ThreadStatus::Active {
            active_flags: vec![]
        }
    );
    for terminal in 0..4 {
        manager.note_turn_started(thread_id).await;
        manager
            .note_background_completion_waiting(thread_id, /*waiting*/ true)
            .await;
        match terminal {
            0 => {
                manager
                    .note_turn_completed(thread_id, /*_failed*/ false)
                    .await
            }
            1 => manager.note_turn_interrupted(thread_id).await,
            2 => manager.note_thread_shutdown(thread_id).await,
            3 => manager.note_system_error(thread_id).await,
            _ => unreachable!(),
        }
        manager
            .note_background_completion_waiting(thread_id, /*waiting*/ true)
            .await;
        assert_eq!(
            manager.loaded_status_for_thread(thread_id).await,
            match terminal {
                0 | 1 => ThreadStatus::Idle,
                2 => ThreadStatus::NotLoaded,
                3 => ThreadStatus::SystemError,
                _ => unreachable!(),
            }
        );
    }
}
