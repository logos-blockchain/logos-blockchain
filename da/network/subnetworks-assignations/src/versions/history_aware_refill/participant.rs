use std::cmp::Ordering;

#[derive(Copy, Clone, PartialEq, Eq)]
pub(super) struct Participant<Id> {
    pub participation: usize,
    pub declaration_id: Id,
}

impl<Id: PartialOrd + Ord> PartialOrd for Participant<Id> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<Id: Ord> Ord for Participant<Id> {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.participation, &self.declaration_id)
            .cmp(&(other.participation, &other.declaration_id))
    }
}
