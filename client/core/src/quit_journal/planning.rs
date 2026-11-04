use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub rows: u64,
    pub bytes: u64,
    pub summaries: u64,
    pub exchanges: u32,
}
impl Usage {
    fn valid(&self) -> bool {
        self.rows <= RETAINED_ROWS
            && self.bytes <= CHARGED_BYTES
            && self.summaries <= RETAINED_SUMMARIES
            && self.exchanges <= EXCHANGES
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotPhase {
    ReservedSummary,
    CustodyPreparing,
    CustodyBound,
    WorkerSpawnAttempted,
    WorkerObserved,
    AdmissionClosed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub binding: Binding,
    pub phase: SlotPhase,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Global {
    pub version: u32,
    pub revision: u64,
    pub usage: Usage,
    #[serde(deserialize_with = "present_option")]
    pub slot: Option<Slot>,
}
impl Global {
    fn validate(&self) -> Result<(), Error> {
        check(
            self.version == VERSION
                && self.revision <= MAX_REVISION
                && self.usage.valid()
                && (self.slot.is_some() || self.usage.exchanges == 0)
                && self.slot.as_ref().is_none_or(|slot| slot.binding.valid()),
        )
    }
    fn next(&self) -> Result<Self, Error> {
        self.validate()?;
        let mut next = self.clone();
        next.revision = next
            .revision
            .checked_add(1)
            .filter(|n| *n <= MAX_REVISION)
            .ok_or(Error::Capacity)?;
        Ok(next)
    }
}

/// A future store must perform these bounded indexed checks INSIDE its real
/// transaction. A supplied implementation/Usage is not global uniqueness proof.
pub trait Retained {
    fn request(&mut self, id: &str) -> Result<bool, Error>;
    fn target(&mut self, target: &Target) -> Result<bool, Error>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupBinding {
    pub record: Binding,
    pub id: String,
    pub ordinal: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberBinding {
    pub group: GroupBinding,
    pub ordinal: u32,
    pub request_id: String,
    pub descriptor_sha256: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberPhase {
    Pending,
    NativeSpawnAttempted,
    Completed,
    Failed,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberState {
    pub binding: MemberBinding,
    pub phase: MemberPhase,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub binding: GroupBinding,
    pub total: u32,
    pub issued: u32,
    pub not_issued: u32,
}

impl Record {
    pub fn group_binding(&self, ordinal: u32) -> Result<GroupBinding, Error> {
        let g = self.groups.get(ordinal as usize).ok_or(Error::Invalid)?;
        Ok(GroupBinding {
            record: self.binding(),
            id: g.id.clone(),
            ordinal,
        })
    }
    pub fn member_binding(&self, group: u32, member: u32) -> Result<MemberBinding, Error> {
        let (_, m) = self.member(group, member)?;
        Ok(MemberBinding {
            group: self.group_binding(group)?,
            ordinal: member,
            request_id: m.descriptor.request_id.clone(),
            descriptor_sha256: m.planning_sha256.clone(),
        })
    }
    fn checked_progress(&self, p: &Progress) -> Result<(), Error> {
        let group = self
            .groups
            .get(p.binding.ordinal as usize)
            .ok_or(Error::Invalid)?;
        check(
            p.binding == self.group_binding(p.binding.ordinal)?
                && p.total as usize == group.members.len()
                && p.issued.checked_add(p.not_issued) == Some(p.total),
        )
    }
}

/// Serializable CAS data, never a durable commit result or native issue token.
/// There is intentionally no Deserialize/replay executor or resource release.
#[derive(Clone, Debug, Serialize)]
pub struct ReservePlan {
    pub before: Global,
    pub after: Global,
    pub record: Record,
    pub progress: Vec<Progress>,
}
pub fn reserve(
    record: Record,
    global: &Global,
    observation: &Observation,
    retained: &mut impl Retained,
) -> Result<ReservePlan, Error> {
    global.validate()?;
    record.context.check_window(observation)?;
    if global.slot.is_some() {
        return Err(Error::Busy);
    }
    let rows = record.rows();
    let charge = record.charge()?;
    let mut after = global.next()?;
    after.usage.rows = after.usage.rows.checked_add(rows).ok_or(Error::Capacity)?;
    after.usage.bytes = after
        .usage
        .bytes
        .checked_add(charge)
        .ok_or(Error::Capacity)?;
    after.usage.summaries = after
        .usage
        .summaries
        .checked_add(record.groups.len() as u64)
        .ok_or(Error::Capacity)?;
    if !after.usage.valid() {
        return Err(Error::Capacity);
    }
    for group in &record.groups {
        for member in &group.members {
            let d = &member.descriptor;
            if retained.request(&d.request_id)? || retained.target(&d.captured.target)? {
                return Err(Error::Conflict);
            }
        }
    }
    after.slot = Some(Slot {
        binding: record.binding(),
        phase: SlotPhase::ReservedSummary,
    });
    let progress = record
        .groups
        .iter()
        .map(|g| {
            Ok(Progress {
                binding: record.group_binding(g.ordinal)?,
                total: g.members.len() as u32,
                issued: 0,
                not_issued: g.members.len() as u32,
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(ReservePlan {
        before: global.clone(),
        after,
        record,
        progress,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct IssuePlan {
    pub before: Global,
    pub after: Global,
    pub before_member: MemberState,
    pub after_member: MemberState,
    pub before_progress: Progress,
    pub after_progress: Progress,
}
pub fn issue(
    record: &Record,
    global: &Global,
    member: &MemberState,
    progress: &Progress,
    observation: &Observation,
) -> Result<IssuePlan, Error> {
    global.validate()?;
    record.context.check_window(observation)?;
    let slot = global.slot.as_ref().ok_or(Error::Invalid)?;
    check(
        global.usage.rows >= record.rows()
            && global.usage.bytes >= record.charge()?
            && global.usage.summaries >= record.groups.len() as u64,
    )?;
    check(
        slot.binding == record.binding()
            && member.binding
                == record.member_binding(member.binding.group.ordinal, member.binding.ordinal)?,
    )?;
    if slot.phase != SlotPhase::WorkerObserved || member.phase != MemberPhase::Pending {
        return Err(Error::Spent);
    }
    record.checked_progress(progress)?;
    check(progress.binding == member.binding.group && progress.not_issued > 0)?;
    let mut after = global.next()?;
    after.usage.exchanges = after
        .usage
        .exchanges
        .checked_add(1)
        .filter(|n| *n <= EXCHANGES)
        .ok_or(Error::Capacity)?;
    let mut after_member = member.clone();
    after_member.phase = MemberPhase::NativeSpawnAttempted;
    let mut after_progress = progress.clone();
    after_progress.issued = after_progress
        .issued
        .checked_add(1)
        .ok_or(Error::Capacity)?;
    after_progress.not_issued -= 1;
    Ok(IssuePlan {
        before: global.clone(),
        after,
        before_member: member.clone(),
        after_member,
        before_progress: progress.clone(),
        after_progress,
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct ClosePlan {
    pub before: Global,
    pub after: Global,
    pub progress: Vec<Progress>,
}
/// Close NEW admissions only, even on expiry/boot mismatch. Does not mark a
/// worker ended, settle a member, deliver a notice or free any charge/resource.
pub fn close(
    record: &Record,
    global: &Global,
    progress: Vec<Progress>,
) -> Result<ClosePlan, Error> {
    global.validate()?;
    let slot = global.slot.as_ref().ok_or(Error::Invalid)?;
    check(slot.binding == record.binding() && progress.len() == record.groups.len())?;
    if slot.phase == SlotPhase::AdmissionClosed {
        return Err(Error::Spent);
    }
    for (index, p) in progress.iter().enumerate() {
        check(p.binding.ordinal as usize == index)?;
        record.checked_progress(p)?;
    }
    let mut after = global.next()?;
    after.slot.as_mut().ok_or(Error::Invalid)?.phase = SlotPhase::AdmissionClosed;
    Ok(ClosePlan {
        before: global.clone(),
        after,
        progress,
    })
}
