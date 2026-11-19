//! WHERE WHAT THE DISK HOLDS WENT IN THE SANDBOX, so an apply takes none of
//! it away while a write that carries it elsewhere has not landed.
//!
//! An apply writes first and takes away last (`apply.rs`). A change that
//! takes away what the disk holds at its path (a delete, a file written
//! over, a directory become a file or the other way round) waits as a
//! conflict while a write of this apply that did not land (skipped or
//! failed) carries some of that elsewhere:
//! - by a move or a copy the sandbox made from the path, from under it, or
//!   of a directory holding it, followed from there in the order the
//!   sandbox made them (`mv a b && mv b c` carries `a` to `c`; an edit
//!   after the move does not matter: `mv a b && echo x >> b`);
//! - or by its content alone: the write would put a file with the content
//!   of one the change takes away (`cat a > b && rm a`). An empty file has
//!   none, so it carries no other empty file.
//!
//! Else `mv src.md locked/`, with `locked` read-only, deleted `src.md` and
//! failed to write `locked/src.md`: its content was in the sandbox alone.
//! The changes that take away are put in an order that tries each write
//! carrying their content first ([`in_order`]); a cycle of them (a swap,
//! `mv a t && mv b a && mv t b`) goes in path order, and where its second
//! write then fails, the first has already gone over what it carried.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    ops::Bound,
    path::{Path, PathBuf},
};

use super::{Scanned, state::Original};

/// The sandbox's moves and copies, `(from, to)`, in the order it made them.
pub(super) struct Carried {
    moves: Vec<(PathBuf, PathBuf)>,
    /// Each move's place in `moves`, by where it came from.
    by_from: BTreeMap<PathBuf, Vec<usize>>,
}

impl Carried {
    pub(super) fn new(moves: Vec<(PathBuf, PathBuf)>) -> Self {
        let mut by_from: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        for (i, (from, _)) in moves.iter().enumerate() {
            by_from.entry(from.clone()).or_default().push(i);
        }
        Self { moves, by_from }
    }

    /// The moves from `at`, from above it or from under it, from the
    /// `first` on, in the order the sandbox made them.
    fn moves_near(&self, at: &Path, first: usize) -> Vec<usize> {
        let above = at.ancestors().filter_map(|a| self.by_from.get(a));
        let under = self
            .by_from
            .range::<Path, _>((Bound::Excluded(at), Bound::Unbounded))
            .take_while(|(from, _)| from.starts_with(at))
            .map(|(_, at)| at);
        let mut all: Vec<usize> = above
            .chain(under)
            .flatten()
            .copied()
            .filter(|&i| i >= first)
            .collect();
        all.sort_unstable();
        all
    }

    /// Each path the sandbox moved or copied what the disk holds at
    /// `start`, or some of it, to; `start` not counted.
    pub(super) fn reached(&self, start: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if self.moves.is_empty() {
            return out;
        }
        // Each path to follow, with the first move that may carry it on.
        let mut pending = vec![(start.to_path_buf(), 0)];
        let mut seen: HashSet<(PathBuf, usize)> = HashSet::new();
        while let Some((at, first)) = pending.pop() {
            for i in self.moves_near(&at, first) {
                let (from, to) = &self.moves[i];
                let next = match at.strip_prefix(from) {
                    Ok(rest) if rest.as_os_str().is_empty() => to.clone(),
                    Ok(rest) => to.join(rest),
                    // Part of what is under `at` went.
                    Err(_) => to.clone(),
                };
                if seen.insert((next.clone(), i + 1)) {
                    out.push(next.clone());
                    pending.push((next, i + 1));
                }
            }
        }
        out
    }
}

/// The writes of an apply that did not land: by path, and by what each
/// would have put there (a file's length and content).
#[derive(Default)]
pub(super) struct Waiting {
    paths: BTreeSet<PathBuf>,
    files: HashSet<(u64, u64)>,
    lengths: HashSet<u64>,
}

impl Waiting {
    /// The write of `s` did not land; `len` is the length of the file it
    /// would have put, where it puts one. An empty file has no content to
    /// carry: by content it would hold the take of every empty file.
    pub(super) fn add(&mut self, s: &Scanned, len: Option<u64>) {
        self.paths.insert(s.path.clone());
        if let (Original::File(hash), Some(len)) = (s.mine, len)
            && !s.mine.is_empty_file()
        {
            self.files.insert((len, hash));
            self.lengths.insert(len);
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Whether a write that did not land is at one of `reached`, under it
    /// or above it.
    pub(super) fn at_any(&self, reached: &[PathBuf]) -> bool {
        reached.iter().any(|r| {
            r.ancestors().any(|a| self.paths.contains(a))
                || self
                    .paths
                    .range::<Path, _>((Bound::Excluded(r.as_path()), Bound::Unbounded))
                    .next()
                    .is_some_and(|p| p.starts_with(r))
        })
    }

    /// Whether a file of `len` bytes may be one a write that did not land
    /// would have put: worth reading to tell.
    pub(super) fn may_hold(&self, len: u64) -> bool {
        self.lengths.contains(&len)
    }

    /// Whether a write that did not land would have put a file of `len`
    /// bytes with content `hash`.
    pub(super) fn holds(&self, len: u64, hash: u64) -> bool {
        self.files.contains(&(len, hash))
    }
}

/// Past this many changes to one content, it ties no change to another:
/// any of them could carry it.
const SAME_CONTENT: usize = 64;

/// `takes`, in path order, in the order to put them: each after every other
/// one its content is carried to (by `carried`, or by the content that one
/// puts), so whether that write landed is known before it is taken away.
/// Where none is free to go (a cycle), the first one left in path order
/// goes.
pub(super) fn in_order(takes: Vec<Scanned>, carried: &Carried) -> Vec<Scanned> {
    let n = takes.len();
    if n < 2 {
        return takes;
    }
    let index: BTreeMap<&Path, usize> = takes
        .iter()
        .enumerate()
        .map(|(i, s)| (s.path.as_path(), i))
        .collect();
    let mut by_content: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, s) in takes.iter().enumerate() {
        // An empty file carries nothing: tied by it, a take could go before
        // the write that carries its content.
        if let Original::File(hash) = s.mine
            && !s.mine.is_empty_file()
        {
            by_content.entry(hash).or_default().push(i);
        }
    }
    // after[i]: each take `i` goes after.
    let mut after: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (i, s) in takes.iter().enumerate() {
        for r in carried.reached(&s.path) {
            let under = index
                .range::<Path, _>((Bound::Included(r.as_path()), Bound::Unbounded))
                .take_while(|(p, _)| p.starts_with(&r))
                .map(|(_, &j)| j);
            let above = r.ancestors().filter_map(|a| index.get(a).copied());
            after[i].extend(under.chain(above));
        }
        if let Original::File(hash) = s.now
            && let Some(same) = by_content.get(&hash)
            && same.len() <= SAME_CONTENT
        {
            after[i].extend(same);
        }
        after[i].remove(&i);
    }
    if after.iter().all(BTreeSet::is_empty) {
        return takes;
    }
    // Kahn's: the first one free to go in path order, each time.
    let mut left: Vec<usize> = after.iter().map(BTreeSet::len).collect();
    let mut frees: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, before) in after.iter().enumerate() {
        for &j in before {
            frees[j].push(i);
        }
    }
    let mut free: BTreeSet<usize> = (0..n).filter(|&i| left[i] == 0).collect();
    let mut done = vec![false; n];
    let mut order = Vec::with_capacity(n);
    while order.len() < n {
        let i = free
            .pop_first()
            .or_else(|| (0..n).find(|&i| !done[i]))
            .expect("one is left while the order is short");
        if done[i] {
            continue;
        }
        done[i] = true;
        order.push(i);
        for &k in &frees[i] {
            left[k] -= 1;
            if left[k] == 0 && !done[k] {
                free.insert(k);
            }
        }
    }
    let mut slots: Vec<Option<Scanned>> = takes.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|i| slots[i].take().expect("each take is ordered once"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moves(pairs: &[(&str, &str)]) -> Carried {
        Carried::new(
            pairs
                .iter()
                .map(|(f, t)| (PathBuf::from(f), PathBuf::from(t)))
                .collect(),
        )
    }

    fn reached(c: &Carried, start: &str) -> Vec<String> {
        let mut all: Vec<_> = c
            .reached(Path::new(start))
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        all.sort();
        all
    }

    #[test]
    fn a_path_is_followed_through_each_later_move_and_copy() {
        let c = moves(&[("/a", "/b"), ("/b", "/c"), ("/c", "/d/c")]);
        assert_eq!(reached(&c, "/a"), ["/b", "/c", "/d/c"]);
        // A move made before the content got there does not carry it.
        let c = moves(&[("/b", "/c"), ("/a", "/b")]);
        assert_eq!(reached(&c, "/a"), ["/b"]);
    }

    #[test]
    fn a_directory_carries_what_is_under_it_and_a_file_under_one_its_part() {
        let c = moves(&[("/lib", "/src")]);
        assert_eq!(reached(&c, "/lib/a.rs"), ["/src/a.rs"]);
        let c = moves(&[("/lib/a.rs", "/keep.rs")]);
        assert_eq!(reached(&c, "/lib"), ["/keep.rs"]);
        let c = moves(&[("/x", "/y")]);
        assert!(reached(&c, "/lib").is_empty());
    }

    #[test]
    fn moves_into_a_directory_and_out_again_end() {
        // `mv a b && mkdir a && mv b a/c`, over and over: each step is a
        // later move, so the walk ends.
        let c = moves(&[("/a", "/b"), ("/b", "/a/c"), ("/a", "/b"), ("/b", "/a/c")]);
        assert!(!reached(&c, "/a").is_empty());
    }
}
