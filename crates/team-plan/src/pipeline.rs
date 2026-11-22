//! What happens after a member reports and after a stage gives its verdict.

use serde::{Deserialize, Serialize};

use crate::catalog::{Pipeline, StageKind};
use crate::model::{Acceptance, Node, NodeKind};
use crate::ready::{PlanView, Staffing, Work};

/// The next step of a task in its pipeline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Hand the result to the stage at `index` of the pipeline.
    Stage {
        index: u32,
        kind: StageKind,
        role: String,
    },
    /// The lead decides, for the reason given.
    ToLead { reason: String },
    /// The person decides.
    ToPerson,
    /// Every stage passed and the pipeline accepts on its own.
    Accept,
    /// Back to the member who implemented the task, which enters `round`.
    ReturnToImplementer { round: u32 },
}

/// The step after the implementer of `node` reported: the first stage, or
/// the pipeline's acceptance when it has no stage.
pub fn after_report(node: &Node, pipeline: &Pipeline) -> Step {
    if node.kind == NodeKind::Stage {
        return to_lead("A stage gives a verdict; it does not report a result.");
    }
    stage_step(pipeline, 0).unwrap_or_else(|| acceptance(pipeline, false))
}

/// The step after `stage`, a stage node checking `node`, gave its verdict.
///
/// A pass goes to the next stage, or to the pipeline's acceptance after the
/// last. A return goes back to the implementer for the next round, and to
/// the lead when the task has used the pipeline's rounds. A stage node
/// carries the round of the report it checks; a verdict that does not fit
/// the task (another task, another round, a stage the pipeline lacks, or a
/// role other than that stage's) goes to the lead and moves nothing.
pub fn after_verdict(node: &Node, stage: &Node, pass: bool, pipeline: &Pipeline) -> Step {
    if stage.kind != NodeKind::Stage {
        return to_lead("The verdict does not come from a stage.");
    }
    if stage.stage_of != Some(node.id) {
        return to_lead("The stage checks another task.");
    }
    let round = node.round.max(1);
    if stage.round.max(1) != round {
        return to_lead(format!(
            "The verdict is for round {} and the task is in round {round}.",
            stage.round.max(1)
        ));
    }
    let number = stage.stage_index.saturating_add(1);
    let spec = usize::try_from(stage.stage_index)
        .ok()
        .and_then(|index| pipeline.stages.get(index));
    let Some(spec) = spec else {
        return to_lead(format!("The pipeline has no stage {number}."));
    };
    if stage.role != spec.role {
        return to_lead(format!(
            "The verdict comes from the role {}, and stage {number} of the pipeline is for the role {}.",
            stage.role, spec.role
        ));
    }
    if pass {
        return stage_step(pipeline, stage.stage_index + 1)
            .unwrap_or_else(|| acceptance(pipeline, true));
    }
    let rounds = pipeline.rounds();
    if round < rounds {
        Step::ReturnToImplementer { round: round + 1 }
    } else {
        to_lead(format!(
            "The task was returned in each of its {rounds} rounds."
        ))
    }
}

/// `step`, unless it is a stage that no member of the team who is not
/// removed could take for `node`: then the lead decides, with the reason.
pub fn staffed(step: Step, node: &Node, view: &PlanView) -> Step {
    let Step::Stage { role, .. } = &step else {
        return step;
    };
    // zerus-63rru.20 round 2: at stage creation the node passed is the
    // target task itself; its recorded earlier implementers are attached by
    // the ops layer and read here, together with its current implementer.
    let mut implementers: Vec<&str> = node.implementer.as_deref().into_iter().collect();
    implementers.extend(node.earlier_implementers.iter().map(|m| m.as_str()));
    let work = Work {
        role,
        assignee: None,
        implementers,
    };
    match (Staffing { view: *view }).gap(&work) {
        Some(reason) => Step::ToLead { reason },
        None => step,
    }
}

fn to_lead(reason: impl Into<String>) -> Step {
    Step::ToLead {
        reason: reason.into(),
    }
}

/// Stage `index` of the pipeline, if it has one.
fn stage_step(pipeline: &Pipeline, index: u32) -> Option<Step> {
    let stage = pipeline.stages.get(usize::try_from(index).ok()?)?;
    Some(Step::Stage {
        index,
        kind: stage.kind,
        role: stage.role.clone(),
    })
}

/// Advance past disabled Land steps without recording a verdict for them.
pub fn after_skipping_land(pipeline: &Pipeline, mut index: u32, passed: bool) -> Step {
    while pipeline
        .stages
        .get(index as usize)
        .is_some_and(|stage| stage.kind == StageKind::Land)
    {
        index += 1;
    }
    stage_step(pipeline, index).unwrap_or_else(|| match acceptance(pipeline, passed) {
        Step::ToLead { .. } => {
            to_lead("Land stages were skipped because landing is disabled in Team settings.")
        }
        step => step,
    })
}

/// Who accepts once nothing is left to check. A pipeline accepts on its own
/// only when a stage passed: one without stages leaves it to the lead.
fn acceptance(pipeline: &Pipeline, passed: bool) -> Step {
    match (pipeline.accept, passed) {
        (Acceptance::Person, _) => Step::ToPerson,
        (Acceptance::Pipeline, true) => Step::Accept,
        (Acceptance::Pipeline, false) => {
            to_lead("The pipeline accepts when its stages pass, and it has none.")
        }
        (Acceptance::Lead, true) => to_lead("Every stage passed."),
        (Acceptance::Lead, false) => to_lead("The task is reported and its pipeline has no stage."),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::catalog::DEFAULT_ROUNDS;
    use crate::model::NodeId;
    use crate::testkit::{builtin_pipeline, member, node, Plan};

    /// Task 1, reported by `m1` in `round`.
    fn task(round: u32) -> Node {
        node(
            1,
            json!({"state": "reported", "implementer": "m1", "round": round, "pipeline": "code"}),
        )
    }

    /// Stage `index` of task 1 in `round`, with the role the built-in
    /// pipelines give that stage: a reviewer, then a tester.
    fn stage_of(index: u32, round: u32) -> Node {
        let role = ["reviewer", "tester"]
            .get(index as usize)
            .copied()
            .unwrap_or("reviewer");
        node(
            10 + index as NodeId,
            json!({"kind": "stage", "stage_of": 1, "stage_index": index, "round": round,
                   "role": role, "implementer": "m1"}),
        )
    }

    fn stage(index: u32, kind: StageKind, role: &str) -> Step {
        Step::Stage {
            index,
            kind,
            role: role.to_owned(),
        }
    }

    fn to_lead(step: &Step) -> &str {
        match step {
            Step::ToLead { reason } => reason,
            other => panic!("not to the lead: {other:?}"),
        }
    }

    #[test]
    fn a_report_goes_to_the_first_stage_of_each_built_in_pipeline() {
        for id in ["code", "reviewed", "docs"] {
            assert_eq!(
                after_report(&task(1), &builtin_pipeline(id)),
                stage(0, StageKind::Review, "reviewer"),
                "{id}"
            );
        }
        // `check` has no stage: the lead decides.
        to_lead(&after_report(&task(1), &builtin_pipeline("check")));
    }

    #[test]
    fn code_passes_review_then_test_then_goes_to_the_lead() {
        let code = builtin_pipeline("code");
        assert_eq!(
            after_verdict(&task(1), &stage_of(0, 1), true, &code),
            stage(1, StageKind::Test, "tester")
        );
        to_lead(&after_verdict(&task(1), &stage_of(1, 1), true, &code));
    }

    #[test]
    fn reviewed_goes_to_the_lead_after_its_review() {
        let reviewed = builtin_pipeline("reviewed");
        to_lead(&after_verdict(&task(1), &stage_of(0, 1), true, &reviewed));
    }

    #[test]
    fn docs_is_accepted_when_its_review_passes() {
        let docs = builtin_pipeline("docs");
        assert_eq!(
            after_verdict(&task(1), &stage_of(0, 1), true, &docs),
            Step::Accept
        );
    }

    #[test]
    fn check_has_no_stage_to_give_a_verdict() {
        let check = builtin_pipeline("check");
        to_lead(&after_verdict(&task(1), &stage_of(0, 1), true, &check));
    }

    #[test]
    fn the_person_decides_when_the_pipeline_says_so() {
        let mut pipeline = builtin_pipeline("reviewed");
        pipeline.accept = Acceptance::Person;
        assert_eq!(
            after_verdict(&task(1), &stage_of(0, 1), true, &pipeline),
            Step::ToPerson
        );
        pipeline.stages.clear();
        assert_eq!(after_report(&task(1), &pipeline), Step::ToPerson);
    }

    #[test]
    fn a_pipeline_without_stages_never_accepts_on_its_own() {
        let mut pipeline = builtin_pipeline("check");
        pipeline.accept = Acceptance::Pipeline;
        to_lead(&after_report(&task(1), &pipeline));
    }

    #[test]
    fn the_third_return_goes_to_the_lead_after_three_rounds() {
        for id in ["code", "reviewed", "docs"] {
            let pipeline = builtin_pipeline(id);
            assert_eq!(pipeline.rounds(), DEFAULT_ROUNDS);
            // Every stage returns the task, round after round.
            let mut round = 1;
            let mut returns = 0;
            let last = loop {
                assert_eq!(
                    after_report(&task(round), &pipeline),
                    stage(0, StageKind::Review, "reviewer")
                );
                match after_verdict(&task(round), &stage_of(0, round), false, &pipeline) {
                    Step::ReturnToImplementer { round: next } => {
                        assert_eq!(next, round + 1, "{id}");
                        assert!(next <= DEFAULT_ROUNDS, "{id}: returned past the limit");
                        round = next;
                        returns += 1;
                    }
                    other => break other,
                }
            };
            assert_eq!(
                (returns, round),
                (2, 3),
                "{id}: two returns, then the third round"
            );
            assert!(to_lead(&last).contains('3'), "{id}: {last:?}");
        }
    }

    #[test]
    fn a_pipeline_may_set_its_own_rounds() {
        let mut pipeline = builtin_pipeline("reviewed");
        pipeline.max_rounds = 1;
        to_lead(&after_verdict(&task(1), &stage_of(0, 1), false, &pipeline));
        pipeline.max_rounds = 5;
        assert_eq!(
            after_verdict(&task(4), &stage_of(0, 4), false, &pipeline),
            Step::ReturnToImplementer { round: 5 }
        );
        to_lead(&after_verdict(&task(5), &stage_of(0, 5), false, &pipeline));
    }

    #[test]
    fn a_verdict_that_does_not_fit_the_task_goes_to_the_lead() {
        let code = builtin_pipeline("code");
        // A late verdict of an earlier round.
        to_lead(&after_verdict(&task(2), &stage_of(0, 1), true, &code));
        to_lead(&after_verdict(&task(2), &stage_of(0, 1), false, &code));
        // A stage of another task.
        let mut other = stage_of(0, 1);
        other.stage_of = Some(7);
        to_lead(&after_verdict(&task(1), &other, true, &code));
        // A stage the pipeline does not have, even where a pass would end it.
        to_lead(&after_verdict(&task(1), &stage_of(2, 1), true, &code));
        let docs = builtin_pipeline("docs");
        to_lead(&after_verdict(&task(1), &stage_of(1, 1), true, &docs));
        to_lead(&after_verdict(&task(1), &stage_of(1, 1), false, &docs));
        // A task that is no stage.
        let mut not_a_stage = stage_of(0, 1);
        not_a_stage.kind = crate::model::NodeKind::Task;
        to_lead(&after_verdict(&task(1), &not_a_stage, true, &code));
        // A stage reports no result.
        to_lead(&after_report(&stage_of(0, 1), &code));
    }

    #[test]
    fn a_verdict_from_a_role_the_stage_does_not_have_goes_to_the_lead() {
        // A developer's pass on the review of a docs task would accept it.
        let docs = builtin_pipeline("docs");
        let mut wrong = stage_of(0, 1);
        wrong.role = "developer".into();
        assert_eq!(
            to_lead(&after_verdict(&task(1), &wrong, true, &docs)),
            "The verdict comes from the role developer, and stage 1 of the pipeline \
             is for the role reviewer."
        );
        to_lead(&after_verdict(&task(1), &wrong, false, &docs));
        // The test stage of code is the tester's, not the reviewer's.
        let code = builtin_pipeline("code");
        let mut reviewer_on_test = stage_of(1, 1);
        assert_eq!(reviewer_on_test.role, "tester");
        assert!(matches!(
            after_verdict(&task(1), &reviewer_on_test, true, &code),
            Step::ToLead { .. }
        ));
        reviewer_on_test.role = "reviewer".into();
        to_lead(&after_verdict(&task(1), &reviewer_on_test, true, &code));
    }

    #[test]
    fn review_focus_4_a_stage_only_its_implementer_could_do_goes_to_the_lead() {
        // The reviewer m3 implemented the task and is the only reviewer.
        let mut implemented = task(1);
        implemented.implementer = Some("m3".into());
        implemented.role = "reviewer".into();
        let mut plan = Plan::new(vec![implemented.clone()]);
        let next = after_report(&implemented, &builtin_pipeline("reviewed"));
        let step = staffed(next.clone(), &implemented, &plan.view());
        assert_eq!(
            to_lead(&step),
            "m3 implemented the task and is the only member with the role reviewer, \
             and a stage is never handed to a member who implemented any round of the task. \
             The task went to the lead, who can accept, return or cancel it there."
        );
        // A second reviewer, even busy, can take it later.
        plan.members.push(member("m4", "reviewer"));
        plan.member("m4").free = false;
        assert_eq!(
            staffed(next, &implemented, &plan.view()),
            stage(0, StageKind::Review, "reviewer")
        );
    }

    #[test]
    fn a_stage_of_a_role_the_team_lacks_goes_to_the_lead() {
        let plan = Plan::new(vec![task(1)]);
        let step = after_verdict(&task(1), &stage_of(0, 1), true, &builtin_pipeline("code"));
        assert_eq!(step, stage(1, StageKind::Test, "tester"));
        assert_eq!(
            to_lead(&staffed(step, &task(1), &plan.view())),
            "No member of the team has the role tester."
        );
        // Other steps pass through.
        assert_eq!(staffed(Step::Accept, &task(1), &plan.view()), Step::Accept);
    }

    #[test]
    fn steps_have_a_stable_json_form() {
        let cases = [
            (
                stage(0, StageKind::Review, "reviewer"),
                json!({"step": "stage", "index": 0, "kind": "review", "role": "reviewer"}),
            ),
            (
                Step::ToLead {
                    reason: "Why.".into(),
                },
                json!({"step": "to_lead", "reason": "Why."}),
            ),
            (Step::ToPerson, json!({"step": "to_person"})),
            (Step::Accept, json!({"step": "accept"})),
            (
                Step::ReturnToImplementer { round: 2 },
                json!({"step": "return_to_implementer", "round": 2}),
            ),
        ];
        for (step, form) in cases {
            assert_eq!(serde_json::to_value(&step).unwrap(), form);
            assert_eq!(serde_json::from_value::<Step>(form).unwrap(), step);
        }
        // serde does not refuse unknown fields beside the tag of a unit
        // variant; a step is the kernel's output, never read from a file.
        assert!(serde_json::from_value::<Step>(
            json!({"step": "stage", "index": 0, "kind": "review", "role": "r", "x": 1})
        )
        .is_err());
    }
}
