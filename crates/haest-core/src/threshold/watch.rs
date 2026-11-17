//! [`Thresholds`] (a site's list), [`Site`] (where a list is checked) and
//! [`Watch`] (which have fired).

use std::{fmt, marker::PhantomData};

use super::{Action, Crossing, Meter, Reading, Rearm, ThresholdRef};

/// PLUG POINT: where thresholds are checked, and so what they may read and
/// do. haest-core has [`TurnSite`](super::TurnSite) and
/// [`LedgerSite`](super::LedgerSite), haest-tools `AgentTurnSite`; a host
/// that checks a list of its own names its own.
pub trait Site: 'static {
    /// When the site checks, for an error: "during a turn".
    const NAME: &'static str;
    /// It reads `meter` and can take `action`.
    fn serves(meter: Meter, action: Action) -> bool;
    /// What it checks unless a host says otherwise.
    fn defaults() -> Vec<ThresholdRef>;
}

/// A threshold its site cannot check: it does not read the meter, or cannot
/// take the action.
#[derive(Debug, Clone, Copy, PartialEq, thiserror::Error)]
#[error("a threshold that would {action} on {meter} is not checked {site}")]
pub struct NotServed {
    /// The site ([`Site::NAME`]).
    pub site: &'static str,
    /// The threshold's meter.
    pub meter: Meter,
    /// The threshold's action.
    pub action: Action,
}

/// The thresholds one site checks, in order: the order a site says what is
/// due. `Default` is the site's defaults; cheap to clone.
pub struct Thresholds<S: Site> {
    list: Vec<ThresholdRef>,
    site: PhantomData<fn() -> S>,
}

impl<S: Site> Thresholds<S> {
    /// None at all: the site checks nothing.
    pub fn none() -> Self {
        Self {
            list: Vec::new(),
            site: PhantomData,
        }
    }

    /// Exactly `list`.
    ///
    /// # Errors
    ///
    /// [`NotServed`] for the first threshold this site cannot check.
    pub fn new(list: impl IntoIterator<Item = impl Into<ThresholdRef>>) -> Result<Self, NotServed> {
        list.into_iter().try_fold(Self::none(), Self::try_with)
    }

    /// These, and `threshold` after them.
    ///
    /// # Errors
    ///
    /// [`NotServed`] when this site cannot check it.
    pub fn try_with(mut self, threshold: impl Into<ThresholdRef>) -> Result<Self, NotServed> {
        let threshold = threshold.into();
        let (meter, action) = (threshold.meter(), threshold.action());
        if !S::serves(meter, action) {
            return Err(NotServed {
                site: S::NAME,
                meter,
                action,
            });
        }
        self.list.push(threshold);
        Ok(self)
    }

    /// These, and `more` after them, which the caller knows this site checks.
    pub(crate) fn served(mut self, more: impl IntoIterator<Item = ThresholdRef>) -> Self {
        let more: Vec<_> = more.into_iter().collect();
        debug_assert!(more.iter().all(|t| S::serves(t.meter(), t.action())));
        self.list.extend(more);
        self
    }

    /// Every threshold, in order.
    pub fn iter(&self) -> std::slice::Iter<'_, ThresholdRef> {
        self.list.iter()
    }

    /// How many there are.
    pub fn len(&self) -> usize {
        self.list.len()
    }

    /// There are none.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// The first threshold that takes `action` and is at or past its mark now,
    /// whether or not it fired: a live question ("is this turn winding
    /// down?"), with no state. `read` reads a meter, None where it cannot.
    pub fn past(
        &self,
        read: impl Fn(Meter) -> Option<Reading>,
        action: Action,
    ) -> Option<Crossing> {
        self.list
            .iter()
            .filter(|t| t.action() == action)
            .find_map(|t| {
                let reading = read(t.meter())?;
                t.mark().reached(&reading).then(|| Crossing {
                    threshold: t.clone(),
                    reading,
                })
            })
    }
}

impl<'a, S: Site> IntoIterator for &'a Thresholds<S> {
    type Item = &'a ThresholdRef;
    type IntoIter = std::slice::Iter<'a, ThresholdRef>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<S: Site> Default for Thresholds<S> {
    fn default() -> Self {
        Self {
            list: S::defaults(),
            site: PhantomData,
        }
    }
}

impl<S: Site> Clone for Thresholds<S> {
    fn clone(&self) -> Self {
        Self {
            list: self.list.clone(),
            site: PhantomData,
        }
    }
}

impl<S: Site> fmt::Debug for Thresholds<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.list.iter()).finish()
    }
}

/// A site's thresholds and which of them have fired: what a site keeps
/// between checks. `Default` watches the site's defaults.
///
/// ```
/// use haest_core::threshold::{Action, LedgerSite, Meter, Reading, Scope, Thresholds, Watch};
///
/// // The ledger's 80% warning, at $0.85 of $1.
/// let mut watch = Watch::new(Thresholds::<LedgerSite>::default());
/// let read = |usd: f64| move |m: Meter| {
///     (m == Meter::Usd(Scope::Conversation))
///         .then_some(Reading { meter: m, value: usd, cap: Some(1.0), limit: None })
/// };
/// assert_eq!(watch.check(read(0.85), Action::Warn).len(), 1);
/// assert!(watch.check(read(0.90), Action::Warn).is_empty()); // once per crossing
/// ```
pub struct Watch<S: Site> {
    thresholds: Thresholds<S>,
    fired: Vec<bool>,
}

impl<S: Site> Watch<S> {
    /// Watches `thresholds`, none fired.
    pub fn new(thresholds: Thresholds<S>) -> Self {
        let fired = vec![false; thresholds.len()];
        Self { thresholds, fired }
    }

    /// What it watches.
    pub fn thresholds(&self) -> &Thresholds<S> {
        &self.thresholds
    }

    /// Watch `thresholds` from now on: one it already watched (the same
    /// object, [`ThresholdRef::ptr_eq`]) keeps whether it fired, a new one
    /// starts armed.
    pub fn follow(&mut self, thresholds: &Thresholds<S>) {
        let fired = thresholds.iter().map(|t| {
            let same = |o: &ThresholdRef| ThresholdRef::ptr_eq(o, t);
            let before = self.thresholds.iter().position(same);
            before.is_some_and(|i| self.fired[i])
        });
        self.fired = fired.collect();
        self.thresholds = thresholds.clone();
    }

    /// A new turn starts: every [`Rearm::NextTurn`] threshold re-arms.
    pub fn next_turn(&mut self) {
        for (t, fired) in self.thresholds.list.iter().zip(&mut self.fired) {
            if t.rearm() == Rearm::NextTurn {
                *fired = false;
            }
        }
    }

    /// The thresholds that take `action` and fire now, in order: each at or
    /// past its mark and armed. Each fires per its [`Rearm`]; one whose
    /// meter `read` cannot read (None) is skipped.
    pub fn check(
        &mut self,
        read: impl Fn(Meter) -> Option<Reading>,
        action: Action,
    ) -> Vec<Crossing> {
        let mut due = Vec::new();
        for (t, fired) in self.thresholds.list.iter().zip(&mut self.fired) {
            if t.action() != action {
                continue;
            }
            let Some(reading) = read(t.meter()) else {
                continue;
            };
            let past = t.mark().reached(&reading);
            let fire = match t.rearm() {
                Rearm::EveryCheck => past,
                Rearm::BelowMark if !past => {
                    *fired = false;
                    false
                }
                Rearm::Never | Rearm::NextTurn | Rearm::BelowMark => {
                    let fire = past && !*fired;
                    *fired |= fire;
                    fire
                }
            };
            if fire {
                due.push(Crossing {
                    threshold: t.clone(),
                    reading,
                });
            }
        }
        due
    }

    /// Each threshold, and whether it has fired (and not re-armed since).
    pub(crate) fn fired(&self) -> impl Iterator<Item = (&ThresholdRef, bool)> {
        self.thresholds.list.iter().zip(self.fired.iter().copied())
    }

    /// The `index`th threshold has fired: what a restored site already said.
    pub(crate) fn set_fired(&mut self, index: usize) {
        if let Some(fired) = self.fired.get_mut(index) {
            *fired = true;
        }
    }

    /// Takes these readings as already heard, firing nothing: a threshold at
    /// or past its mark counts as fired, and a [`Rearm::BelowMark`] one under
    /// it re-arms. For a site restored from a log, or a cap the human changed.
    pub fn settle(&mut self, read: impl Fn(Meter) -> Option<Reading>) {
        for (t, fired) in self.thresholds.list.iter().zip(&mut self.fired) {
            let Some(reading) = read(t.meter()) else {
                continue;
            };
            let past = t.mark().reached(&reading);
            match t.rearm() {
                Rearm::BelowMark => *fired = past,
                Rearm::Never | Rearm::NextTurn => *fired |= past,
                Rearm::EveryCheck => {}
            }
        }
    }
}

impl<S: Site> Default for Watch<S> {
    fn default() -> Self {
        Self::new(Thresholds::default())
    }
}

/// Cheap: the thresholds are handles, and the copy keeps which have fired.
impl<S: Site> Clone for Watch<S> {
    fn clone(&self) -> Self {
        Self {
            thresholds: self.thresholds.clone(),
            fired: self.fired.clone(),
        }
    }
}

impl<S: Site> fmt::Debug for Watch<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Watch")
            .field("thresholds", &self.thresholds)
            .field("fired", &self.fired)
            .finish()
    }
}
