use super::*;

/// Operation policy is centralized here. Service facts never inherit Person
/// authority. Native caller and exact persisted session identity are verified
/// separately, before operation authority or a saved receipt is consulted.
pub fn authority(actor: &Actor, op: &Operation, team: &Team) -> Result<()> {
    let denied = || {
        Problem::new(
            code::NOT_ALLOWED,
            "This actor cannot perform this team operation.",
        )
    };
    if actor.person() != team.owner {
        return Err(denied());
    }
    let service = matches!(actor, Actor::Service { team: id, .. } if id == &team.id);
    let person = matches!(actor, Actor::Person { .. });
    let lead = matches!(actor, Actor::Session { team: id, member, .. } if id == &team.id && member == &team.lead);
    let member = matches!(actor, Actor::Session { team: id, member, .. } if id == &team.id && member != &team.lead);
    let allowed = match op {
        Operation::SetPhaseAcceptance { .. } => person,
        Operation::SetPhaseBarrier { .. }
        | Operation::PausePhase { .. }
        | Operation::ResumePhase { .. }
        | Operation::MovePhase { .. } => person || lead,
        Operation::ApprovePhase { .. } | Operation::ApprovePhaseCorrections { .. } => {
            person || service
        }
        Operation::SendBackPhase { .. } | Operation::AcceptPhase { .. } => {
            person || (lead && team.settings.delegations.accept_results)
        }
        Operation::SubmitPhaseCorrections { .. } | Operation::EditPhaseCorrections { .. } => {
            person || lead
        }
        Operation::RejectPhaseCorrections { .. }
        | Operation::UndoPhaseCorrectionRejection { .. } => person,
        Operation::ReconcileCorrectionReview { .. }
        | Operation::ReconcilePhaseReview { .. }
        | Operation::ReconcilePublication { .. }
        | Operation::PublicationTaskProblem { .. }
        | Operation::ReconcilePhase { .. } => service,
        Operation::RejectPhase { .. } | Operation::UndoRejectPhase { .. } => person,
        Operation::LeadAddMember { .. } => lead,
        Operation::Verdict { .. } => member,
        Operation::CloseCard { .. } => service,
        Operation::RecordTimingFact { .. }
        | Operation::ObserveTask { .. }
        | Operation::HandoutIntent { .. }
        | Operation::HandoutOutcome { .. }
        | Operation::ReopenIntent { .. }
        | Operation::ReopenOutcome { .. }
        | Operation::SetMemberSession { .. }
        | Operation::SetMemberHandle { .. }
        | Operation::SetMemberState { .. }
        | Operation::RecordReport { .. }
        | Operation::StageTaskIntent { .. }
        | Operation::StageTaskOutcome { .. }
        | Operation::RecordIntegration { .. }
        | Operation::RecordIntegrationConflict { .. }
        | Operation::FireTimer { .. }
        | Operation::RecordKeyClear { .. }
        | Operation::RecordLateSession { .. }
        | Operation::RecordLateReport { .. }
        | Operation::RecordCancel { .. }
        | Operation::ReconcileLeadQueue
        | Operation::ReconcileUnstaffed
        | Operation::ReleaseProviderStop { .. }
        | Operation::RecordMemberReadability { .. }
        | Operation::EndStaleUnstaffed
        | Operation::CloseStaleReopenNotices
        | Operation::SettleLeftHandout { .. }
        | Operation::UnbindMember { .. }
        | Operation::TellPausedStarted { .. }
        | Operation::ClosePausedStarted { .. } => service,
        Operation::SwapMember { .. }
        | Operation::AddMember { .. }
        | Operation::RenameTeam { .. }
        | Operation::SetGoal { .. }
        | Operation::Create { .. }
        | Operation::StartPlanning { .. }
        | Operation::Stop
        | Operation::Archive
        | Operation::StopMember { .. }
        | Operation::ArchiveMember { .. }
        | Operation::SetValidationConfig { .. }
        | Operation::ChangeSettings { .. }
        | Operation::AdvanceBase { .. }
        | Operation::DecideCard { .. }
        | Operation::SetDeps { .. } => person,
        Operation::AddTask { .. } => true,
        Operation::Ask { .. } => member || person,
        Operation::Accept { .. } => person || (lead && team.settings.delegations.accept_results),
        Operation::Reply { .. } => person || (lead && team.settings.delegations.answer_members),
        // A card's parameters feed the window's own sentence, so only the
        // service's card writer supplies them; a person or the lead may open a
        // card's words, never its substitution values.
        Operation::OpenCard { parameters, .. } => match parameters {
            Some(list) if !list.is_empty() => service,
            _ => person || lead || service,
        },
        Operation::Pause
        | Operation::SubmitDraftPhase { .. }
        | Operation::EditDraftPhase { .. }
        | Operation::Resume
        | Operation::PauseMember { .. }
        | Operation::ResumeMember { .. }
        | Operation::Admit { .. }
        | Operation::Route { .. }
        | Operation::Hold { .. }
        | Operation::Release { .. }
        | Operation::Return { .. }
        | Operation::Reassign { .. }
        | Operation::Cancel { .. } => person || lead,
    };
    if allowed {
        Ok(())
    } else {
        Err(denied())
    }
}
