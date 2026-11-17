//! 按最近用过排的一张表，每项记着有几处在用。多出上限时只挤掉没在用的，从最久没用的起；都在用时暂时多着，
//! 等有一处不用了再挤。

use std::{collections::VecDeque, sync::Arc};

pub(super) struct Slots<V> {
    limit: usize,
    /// 最近用过的在后面。每项的计数表里一份，在用的每处各一份（`touch`、`insert` 交出去的），只剩表里那份时
    /// 没在用。
    slots: VecDeque<(V, Arc<()>)>,
}

impl<V> Slots<V> {
    pub(super) fn new(limit: usize) -> Self {
        Self { limit, slots: VecDeque::new() }
    }

    /// 找到 `matches` 的那项，记作最近用过，返回它和一份在用计数（丢掉就是不用了）。
    pub(super) fn touch(&mut self, matches: impl Fn(&V) -> bool) -> Option<(&V, Arc<()>)> {
        let at = self.slots.iter().position(|(value, _)| matches(value))?;
        let slot = self.slots.remove(at)?;
        self.slots.push_back(slot);
        let (value, count) = self.slots.back()?;
        Some((value, count.clone()))
    }

    /// 加一项，记作最近用过，返回一份在用计数。不挤别的，由调用方 `trim`。
    pub(super) fn insert(&mut self, value: V) -> Arc<()> {
        let count = Arc::new(());
        self.slots.push_back((value, count.clone()));
        count
    }

    /// 拿出 `matches` 的那项，在不在用都拿。
    pub(super) fn remove(&mut self, matches: impl Fn(&V) -> bool) -> Option<V> {
        let at = self.slots.iter().position(|(value, _)| matches(value))?;
        self.slots.remove(at).map(|(value, _)| value)
    }

    /// 多出上限时拿出没在用的，从最久没用的起，直到不再多出或剩下的都在用。
    pub(super) fn trim(&mut self) -> Vec<V> {
        let mut evicted = Vec::new();
        let mut at = 0;
        while self.slots.len() > self.limit && at < self.slots.len() {
            if Arc::strong_count(&self.slots[at].1) == 1 {
                evicted.extend(self.slots.remove(at).map(|(value, _)| value));
            } else {
                at += 1;
            }
        }
        evicted
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(limit: usize, values: impl IntoIterator<Item = u32>) -> (Slots<u32>, Vec<Arc<()>>) {
        let mut slots = Slots::new(limit);
        let counts = values.into_iter().map(|value| slots.insert(value)).collect();
        (slots, counts)
    }

    fn values(slots: &Slots<u32>) -> Vec<u32> {
        slots.slots.iter().map(|(value, _)| *value).collect()
    }

    /// 多出上限时挤掉的是没在用、最久没用的；用过一下的排到后面。
    #[test]
    fn the_idle_one_used_longest_ago_goes_first() {
        let (mut slots, counts) = filled(3, [1, 2, 3]);
        drop(counts);
        let (_, used) = slots.touch(|&value| value == 1).unwrap();
        drop(used);
        let _four = slots.insert(4);
        assert_eq!(slots.trim(), [2]);
        assert_eq!(values(&slots), [3, 1, 4]);
        assert!(slots.trim().is_empty());
    }

    /// 在用的不挤，哪怕它最久没用；挤它后面没在用的。
    #[test]
    fn one_in_use_is_skipped() {
        let (mut slots, mut counts) = filled(2, [1, 2, 3]);
        let one = counts.remove(0);
        drop(counts);
        assert_eq!(slots.trim(), [2]);
        assert_eq!(values(&slots), [1, 3]);
        drop(one);
    }

    /// 都在用时暂时多着；有一处不用了，下次 `trim` 挤掉它。同一项在用几处，要都不用了才算没在用。
    #[test]
    fn all_in_use_stay_until_one_is_done() {
        let (mut slots, mut counts) = filled(2, [1, 2, 3]);
        let (_, again) = slots.touch(|&value| value == 2).unwrap();
        assert!(slots.trim().is_empty());
        assert_eq!(slots.len(), 3);

        drop(counts.remove(1));
        assert!(slots.trim().is_empty(), "2 is still used once more");
        drop(again);
        assert_eq!(slots.trim(), [2]);
        assert_eq!(values(&slots), [1, 3]);
    }

    /// `remove` 在用的也拿出来；没有那项时为 `None`。
    #[test]
    fn remove_takes_one_in_use() {
        let (mut slots, _counts) = filled(2, [1, 2]);
        assert_eq!(slots.remove(|&value| value == 1), Some(1));
        assert_eq!(slots.remove(|&value| value == 1), None);
        assert_eq!(values(&slots), [2]);
    }
}
