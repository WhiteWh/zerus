//! Every session effect and durable boundary is explicit for tests.
use super::types::*;

pub(super) type Result<T> = std::result::Result<T, String>;

pub(super) trait Ports: Sized {
    fn facts(&mut self, binding: &Binding) -> Result<Facts>;
    fn observe(&mut self, binding: &Binding) -> Result<Snapshot>;
    fn observe_member(&mut self, binding: &Binding) -> Result<Snapshot> {
        self.observe(binding)
    }
    fn ownership(&mut self, binding: &Binding) -> Result<Ownership>;
    fn unsettled(&mut self, binding: &Binding) -> Result<Option<Record>>;
    fn pending_brief(&mut self, binding: &Binding) -> Result<Option<Plan>>;
    fn claim(&mut self, plan: &Plan, epoch: &str, now: i64) -> Result<Claim>;
    fn save(&mut self, row: &Record) -> Result<()>;
    fn audit_begin(&mut self, plan: &Plan) -> Result<Audit>;
    fn audit_finish(&mut self, row: &Record) -> Result<()>;
    fn card(&mut self, row: &Record) -> Result<()>;
    fn original(&mut self, row: &Record, check: &mut dyn FnMut(&mut Self) -> Result<()>)
        -> Outcome;
}
