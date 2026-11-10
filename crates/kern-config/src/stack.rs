//! The layers in the order a host added them ([`crate::Loader`]), each read in
//! its turn, then laid over the defaults: a later layer's value wins.

use std::{collections::BTreeMap, path::PathBuf};

use crate::{
    fields::{Config, set},
    registry::{Def, Kind, find},
    source::Source,
    value::{absolute, check},
};

/// Checked values, each with where it came from.
pub(crate) type Tagged = Vec<(&'static Def, String, Source)>;

/// Checked values of one profile table: where they came from is the profile.
pub(crate) type Entries = Vec<(&'static Def, String)>;

/// One value a host handed in, not yet checked: a variable's, a flag's.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Given {
    key: String,
    raw: String,
    source: Source,
}

impl Given {
    pub fn new(key: impl Into<String>, raw: impl Into<String>, source: Source) -> Self {
        Self {
            key: key.into(),
            raw: raw.into(),
            source,
        }
    }
}

/// A layer before it is read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Pending {
    /// Values in hand (variables, flags): checked first.
    Values(Vec<Given>),
    /// A config file. None: `<config dir>/config.toml`, when there is one.
    File(Option<PathBuf>),
    /// The nearest `.kern/config.toml`, inside `trusted_dirs` only.
    Project,
    /// The profile `profile` names.
    Profile,
}

/// A layer read: its values, and the profiles it defines (a file's
/// `[profiles.<name>]` tables).
#[derive(Debug, Clone, Default)]
pub(crate) struct Layer {
    pub values: Tagged,
    pub profiles: BTreeMap<String, Entries>,
}

impl Layer {
    /// Every value checked against its setting; an error names where it came from.
    pub fn checked(given: Vec<Given>) -> Result<Self, String> {
        let values = given
            .into_iter()
            .map(|g| {
                let def = find(&g.key)
                    .ok_or_else(|| format!("{}: unknown setting \"{}\"", g.source, g.key))?;
                let value = check(def, &g.raw).map_err(|e| format!("{} {e}", g.source))?;
                Ok((def, value, g.source))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            values,
            profiles: BTreeMap::new(),
        })
    }
}

#[derive(Debug)]
pub(crate) enum Slot {
    Pending(Pending),
    Read(Layer),
}

/// The layers, in order.
#[derive(Debug, Default)]
pub(crate) struct Stack(pub Vec<Slot>);

impl Stack {
    fn read(&self) -> impl DoubleEndedIterator<Item = &Layer> {
        self.0.iter().filter_map(|slot| match slot {
            Slot::Read(layer) => Some(layer),
            Slot::Pending(_) => None,
        })
    }

    /// Check every layer of values in hand.
    pub fn read_values(&mut self) -> Result<(), String> {
        for slot in &mut self.0 {
            if let Slot::Pending(Pending::Values(given)) = slot {
                *slot = Slot::Read(Layer::checked(std::mem::take(given))?);
            }
        }
        Ok(())
    }

    /// The last value `key` has in the layers read so far.
    pub fn last(&self, key: &str) -> Option<&str> {
        self.read()
            .flat_map(|layer| &layer.values)
            .filter(|(def, _, _)| def.key == key)
            .map(|(_, value, _)| value.as_str())
            .next_back()
    }

    /// The `[profiles.<name>]` tables of the layers read so far, in order.
    pub fn profile_tables(&self, name: &str) -> Vec<&Entries> {
        self.read().filter_map(|l| l.profiles.get(name)).collect()
    }

    /// Every profile name a layer read so far defines.
    pub fn profile_names(&self) -> impl Iterator<Item = &str> {
        self.read()
            .flat_map(|l| l.profiles.keys().map(String::as_str))
    }

    pub fn has(&self, pending: &Pending) -> bool {
        self.0
            .iter()
            .any(|slot| matches!(slot, Slot::Pending(p) if p == pending))
    }

    /// Every layer still `pending` reads `layer`.
    pub fn fill(&mut self, pending: &Pending, layer: &Layer) {
        for slot in &mut self.0 {
            if matches!(slot, Slot::Pending(p) if p == pending) {
                *slot = Slot::Read(layer.clone());
            }
        }
    }

    /// Every layer over `config`, in order, and where each value came from. A
    /// layer still pending read nothing (no project file, no profile named).
    pub fn fold(
        self,
        mut config: Config,
    ) -> Result<(Config, BTreeMap<&'static str, Source>), String> {
        let (cwd, home) = (config.settings.cwd.clone(), config.settings.home.clone());
        let mut sources = BTreeMap::new();
        let values = self.0.into_iter().flat_map(|slot| match slot {
            Slot::Read(layer) => layer.values,
            Slot::Pending(_) => Vec::new(),
        });
        for (def, value, source) in values {
            let value = match def.kind {
                Kind::Path => absolute(&value, &cwd, &home).display().to_string(),
                _ => value,
            };
            set(&mut config, def.key, &value).map_err(|e| format!("{}: {e}", def.key))?;
            sources.insert(def.key, source);
        }
        Ok((config, sources))
    }
}
