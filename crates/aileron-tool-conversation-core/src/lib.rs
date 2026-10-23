//! Executable lifecycle core. Cargo erases specifications; Verus checks this file.
// Keep collection operations on the pinned vstd Vec/String specification surface.
#![allow(clippy::ptr_arg, clippy::len_zero)]
#[cfg(not(verus_keep_ghost))]
use verus_builtin_macros::verus;
#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

verus! {

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnId { serial: u64 }

impl TurnId {
    pub closed spec fn serial(self) -> u64 { self.serial }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Phase { Idle, Running, Waiting }

#[derive(Debug)]
pub struct Conversation {
    serial: u64,
    phase: Phase,
    pending: Vec<String>,
}

pub open spec fn unique(ids: Seq<String>) -> bool {
    forall|i: int, j: int| 0 <= i < j < ids.len() ==> ids[i]@ != ids[j]@
}

pub open spec fn same_ids(a: Seq<String>, b: Seq<String>) -> bool {
    &&& unique(a)
    &&& unique(b)
    &&& (forall|i: int| 0 <= i < a.len() ==> has_id(b, #[trigger] a[i]@))
    &&& (forall|j: int| 0 <= j < b.len() ==> has_id(a, #[trigger] b[j]@))
}

pub open spec fn has_id(ids: Seq<String>, id: Seq<char>) -> bool {
    exists|i: int| 0 <= i < ids.len() && #[trigger] ids[i]@ == id
}

fn contains(ids: &Vec<String>, id: &String) -> (found: bool)
    ensures found == has_id(ids@, id@),
{
    let mut i = 0;
    while i < ids.len()
        invariant i <= ids.len(), forall|j: int| 0 <= j < i ==> ids[j]@ != id@,
        decreases ids.len() - i,
    {
        if ids[i].as_str() == id.as_str() { return true; }
        i += 1;
    }
    false
}

fn valid_ids(ids: &Vec<String>) -> (valid: bool)
    ensures valid ==> unique(ids@),
{
    if ids.len() == 0 || ids.len() > 32 { return false; }
    let mut i = 0;
    while i < ids.len()
        invariant i <= ids.len(),
            forall|a: int, b: int| 0 <= a < b < i ==> ids[a]@ != ids[b]@,
        decreases ids.len() - i,
    {
        if ids[i].as_str() == "" { return false; }
        let mut j = 0;
        while j < i
            invariant j <= i < ids.len(),
                forall|a: int, b: int| 0 <= a < b < i ==> ids[a]@ != ids[b]@,
                forall|a: int| 0 <= a < j ==> ids[a]@ != ids[i as int]@,
            decreases i - j,
        {
            if ids[j].as_str() == ids[i].as_str() { return false; }
            j += 1;
        }
        i += 1;
    }
    true
}

pub fn exact_results(pending: &Vec<String>, results: &Vec<String>) -> (accepted: bool)
    ensures accepted ==> same_ids(pending@, results@),
{
    if pending.len() != results.len() || !valid_ids(pending) || !valid_ids(results) {
        return false;
    }
    let mut i = 0;
    while i < results.len()
        invariant i <= results.len(), unique(pending@), unique(results@),
            forall|j: int| 0 <= j < i ==> has_id(pending@, #[trigger] results[j]@),
        decreases results.len() - i,
    {
        if !contains(pending, &results[i]) { return false; }
        i += 1;
    }
    let mut j = 0;
    while j < pending.len()
        invariant j <= pending.len(), unique(pending@), unique(results@),
            forall|k: int| 0 <= k < results.len() ==> has_id(pending@, #[trigger] results[k]@),
            forall|k: int| 0 <= k < j ==> has_id(results@, #[trigger] pending[k]@),
        decreases pending.len() - j,
    {
        if !contains(results, &pending[j]) { return false; }
        j += 1;
    }
    true
}

impl Conversation {
    pub closed spec fn serial(&self) -> u64 { self.serial }
    pub closed spec fn phase(&self) -> Phase { self.phase }
    pub closed spec fn pending(&self) -> Seq<String> { self.pending@ }

    pub fn new() -> (s: Self)
        ensures s.serial() == 0, s.phase() == Phase::Idle, s.pending().len() == 0,
    { Self { serial: 0, phase: Phase::Idle, pending: Vec::new() } }

    pub fn cancel(&mut self)
        ensures final(self).serial() == old(self).serial(), final(self).phase() == Phase::Idle, final(self).pending().len() == 0,
    {
        self.phase = Phase::Idle;
        self.pending = Vec::new();
    }

    pub fn start(&mut self) -> (turn: Option<TurnId>)
        ensures
            final(self).serial() >= old(self).serial(),
            final(self).pending().len() == 0,
            match turn {
                Some(t) => final(self).serial() == old(self).serial() + 1 && t.serial() == final(self).serial() && final(self).phase() == Phase::Running,
                None => old(self).serial() == u64::MAX && final(self).serial() == u64::MAX && final(self).phase() == Phase::Idle,
            },
    {
        self.cancel();
        if self.serial == u64::MAX { return None; }
        self.serial += 1;
        self.phase = Phase::Running;
        Some(TurnId { serial: self.serial })
    }

    pub fn remember(&mut self, turn: TurnId, ids: Vec<String>) -> (accepted: bool)
        ensures
            accepted ==> old(self).phase() == Phase::Running && turn.serial() == old(self).serial()
                && final(self).phase() == Phase::Waiting && final(self).pending() == ids@ && unique(final(self).pending()),
            !accepted ==> *final(self) == *old(self),
            final(self).serial() == old(self).serial(),
    {
        if !matches!(self.phase, Phase::Running) || turn.serial != self.serial || !valid_ids(&ids) { return false; }
        self.pending = ids;
        self.phase = Phase::Waiting;
        true
    }

    pub fn accepts_results(&self, results: &Vec<String>) -> (accepted: bool)
        ensures accepted ==> self.phase() == Phase::Waiting && same_ids(self.pending(), results@),
    {
        matches!(self.phase, Phase::Waiting) && exact_results(&self.pending, results)
    }

    pub fn continue_with(&mut self, results: &Vec<String>) -> (turn: Option<TurnId>)
        ensures
            match turn {
                Some(t) => old(self).phase() == Phase::Waiting && same_ids(old(self).pending(), results@)
                    && final(self).serial() == old(self).serial() + 1 && t.serial() == final(self).serial()
                    && final(self).phase() == Phase::Running && final(self).pending().len() == 0,
                None => *final(self) == *old(self),
            },
    {
        if !self.accepts_results(results) || self.serial == u64::MAX { return None; }
        self.start()
    }

    // A separate `done` event after tool_calls must preserve the waiting batch.
    pub fn complete(&mut self, turn: TurnId) -> (accepted: bool)
        ensures
            accepted == (old(self).phase() != Phase::Idle && turn.serial() == old(self).serial()),
            !accepted ==> *final(self) == *old(self),
            final(self).serial() == old(self).serial(),
            old(self).phase() == Phase::Waiting ==> *final(self) == *old(self),
            accepted && old(self).phase() == Phase::Running ==> final(self).phase() == Phase::Idle && final(self).pending().len() == 0,
    {
        if matches!(self.phase, Phase::Idle) || turn.serial != self.serial { return false; }
        if matches!(self.phase, Phase::Running) { self.cancel(); }
        true
    }
}
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn exhaustion_never_reuses_a_turn_even_after_cancel() {
        let mut state = Conversation {
            serial: u64::MAX - 1,
            phase: Phase::Idle,
            pending: vec![],
        };
        let last = state.start().unwrap();
        assert_eq!(last.serial, u64::MAX);
        assert!(state.remember(last, vec!["a".into()]));
        assert!(state.continue_with(&vec!["a".into()]).is_none());
        assert!(state.accepts_results(&vec!["a".into()]));
        state.cancel();
        assert!(!state.complete(last));
        assert!(!state.remember(last, vec!["a".into()]));
        assert!(state.start().is_none());
        state.cancel();
        assert!(state.start().is_none());
        assert_eq!(state.serial, u64::MAX);
    }

    #[test]
    fn exact_matching_agrees_with_independent_set_oracle() {
        let mut lists = vec![vec![]];
        for length in 1..=3 {
            for mut code in 0..3usize.pow(length) {
                let mut list = Vec::new();
                for _ in 0..length {
                    list.push(["a", "b", "c"][code % 3].to_owned());
                    code /= 3;
                }
                lists.push(list);
            }
        }
        for a in &lists {
            for b in &lists {
                let sa: BTreeSet<_> = a.iter().collect();
                let sb: BTreeSet<_> = b.iter().collect();
                let expected =
                    !a.is_empty() && sa.len() == a.len() && sb.len() == b.len() && sa == sb;
                assert_eq!(exact_results(a, b), expected, "{a:?} / {b:?}");
            }
        }
    }
}
