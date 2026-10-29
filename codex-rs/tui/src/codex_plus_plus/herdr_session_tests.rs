use codex_protocol::ThreadId;
use crossterm::Command;
use pretty_assertions::assert_eq;

use super::super::CachedStatus;
use super::super::ClearTerminalProgramStatus;
use super::super::ProgramStatus;
use super::super::ProgramStatusKind;
use super::super::SessionStatusState;
use super::super::SetTerminalProgramStatus;

#[test]
fn primary_identity_changes_reemit_idle_and_survive_status_clear() {
    let state = SessionStatusState::new();
    let first = ThreadId::from_u128(/*value*/ 1);
    let next = ThreadId::from_u128(/*value*/ 2);
    let idle = CachedStatus::Reported(ProgramStatus::Idle);
    let mut reports = Vec::new();
    for session in [first, first, next] {
        state.set_herdr_session(Some(session));
        state
            .update(idle, "", |emitted| {
                reports.push(emitted.herdr_session);
                Ok(())
            })
            .unwrap();
    }
    state.update(CachedStatus::Cleared, "", |_| Ok(())).unwrap();
    state
        .update(idle, "", |emitted| {
            reports.push(emitted.herdr_session);
            Ok(())
        })
        .unwrap();
    assert_eq!(reports, [Some(first), Some(next), Some(next)]);
}

#[test]
fn every_root_report_preserves_session_without_changing_record_address_or_clear() {
    let session = ThreadId::from_u128(/*value*/ 42);
    for (status, fields) in [
        (ProgramStatus::Idle, "state=idle:app=codex"),
        (ProgramStatus::Working, "state=working:app=codex"),
        (
            ProgramStatus::Blocked(ProgramStatusKind::Permission),
            "state=blocked:app=codex:kind=permission",
        ),
        (
            ProgramStatus::Blocked(ProgramStatusKind::Question),
            "state=blocked:app=codex:kind=question",
        ),
        (
            ProgramStatus::Blocked(ProgramStatusKind::Auth),
            "state=blocked:app=codex:kind=auth",
        ),
        (ProgramStatus::Done, "state=done:app=codex"),
        (ProgramStatus::Error, "state=error:app=codex"),
    ] {
        let mut encoded = String::new();
        SetTerminalProgramStatus {
            status,
            iterm_detail: "",
            emit_iterm_status: false,
            herdr_session: Some(session),
        }
        .write_ansi(&mut encoded)
        .unwrap();
        assert_eq!(
            encoded,
            format!("\x1b]7501;{fields}:session=00000000-0000-0000-0000-00000000002a\x1b\\")
        );
    }
    let mut clear = String::new();
    ClearTerminalProgramStatus {
        emit_iterm_status: false,
    }
    .write_ansi(&mut clear)
    .unwrap();
    assert_eq!(clear, "\x1b]7501;state=clear\x1b\\");
}
