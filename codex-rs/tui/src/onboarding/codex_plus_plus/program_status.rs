//! Safe authentication/trust labels while the existing onboarding screen owns input.

use super::OnboardingScreen;
use super::Step;
use crate::codex_plus_plus::program_status::Kind;
use crate::codex_plus_plus::program_status::State;
use crate::codex_plus_plus::program_status::Status;

impl OnboardingScreen {
    pub(super) fn program_status(&self) -> Status {
        let kind = self.current_steps().last().and_then(|step| match step {
            Step::Auth(_) => Some(Kind::Auth),
            Step::TrustDirectory(_) => Some(Kind::Permission),
            Step::Welcome(_) => None,
        });
        let message = match kind {
            Some(Kind::Auth) => Some("Sign in to Codex".to_string()),
            Some(Kind::Permission) => Some("Directory trust required".to_string()),
            Some(Kind::Question) | None => None,
        };
        Status {
            state: if kind.is_some() {
                State::Blocked
            } else {
                State::Idle
            },
            kind,
            message,
        }
    }
}
