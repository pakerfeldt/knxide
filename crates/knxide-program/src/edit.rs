//! Changing a device's configuration in code: parameter values and group address links as
//! data, checked against the product, on top of what the project stores.
//!
//! The project's [`DeviceData`] is the baseline and is never changed. [`Edits`] holds what is
//! different; [`Edits::apply`] gives the effective data a download is prepared from.

use std::collections::BTreeMap;
use std::fmt;

use knxide_core::GroupAddress;

use crate::config::Configuration;
use crate::device::{DeviceData, GroupAddresses, ObjectLinks, ParameterValue};
use crate::error::Error;
use crate::product::{
    Access, ApplicationProgram, ObjectAttributes, ParameterKind, ParameterRef, ParameterType,
};

/// What is different from the project for one device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Edits {
    /// Parameter values by parameter reference identifier (without the application's
    /// identifier in front).
    pub parameters: BTreeMap<String, String>,
    /// Group addresses by group object reference identifier, in order: the first is the
    /// sending one. An empty list unlinks the object.
    pub links: BTreeMap<String, Vec<GroupAddress>>,
}

/// An edit that cannot be made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The product has no such parameter reference.
    UnknownParameter(String),
    /// The product has no such group object reference.
    UnknownObject(String),
    /// The parameter is not one a user may change.
    NotEditable(String),
    /// The value does not fit the parameter. The string says why, for a person.
    Invalid(String),
    /// The same group address twice on one object.
    DuplicateLink(GroupAddress),
    /// The product or project data cannot be evaluated.
    Data(Error),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownParameter(id) => write!(f, "the product has no parameter {id}"),
            Self::UnknownObject(id) => write!(f, "the product has no group object {id}"),
            Self::NotEditable(what) => write!(f, "{what} cannot be changed"),
            Self::Invalid(why) => f.write_str(why),
            Self::DuplicateLink(address) => {
                write!(f, "{address} is linked to this object already")
            }
            Self::Data(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for EditError {}

impl From<Error> for EditError {
    fn from(error: Error) -> Self {
        Self::Data(error)
    }
}

/// The parameter reference called `reference`, with its position.
fn parameter_ref<'p>(
    program: &'p ApplicationProgram,
    reference: &str,
) -> Result<(usize, &'p ParameterRef), EditError> {
    program
        .parameter_ref_index(reference)
        .and_then(|at| Some((at, program.parameter_refs.get(at)?)))
        .ok_or_else(|| EditError::UnknownParameter(reference.to_string()))
}

/// The label, type and access of a parameter reference.
pub(crate) fn describe<'p>(
    program: &'p ApplicationProgram,
    reference: &'p ParameterRef,
) -> Option<(&'p str, &'p ParameterType, Access, &'p str)> {
    let parameter = program.parameter(&reference.parameter)?;
    let kind = program.parameter_type(&parameter.parameter_type)?;
    let label = reference.text.as_deref().unwrap_or(&parameter.text);
    let access = reference.access.unwrap_or(parameter.access);
    Some((label, kind, access, &parameter.suffix))
}

/// What a parameter is called for a person: its text with the white space tidied; without a
/// text the product's own name for it, and without that its identifier.
pub(crate) fn label(program: &ApplicationProgram, reference: &ParameterRef) -> String {
    let parameter = program.parameter(&reference.parameter);
    let text = reference
        .text
        .as_deref()
        .or(parameter.map(|parameter| parameter.text.as_str()))
        .unwrap_or_default();
    let tidy = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !tidy.is_empty() {
        return tidy;
    }
    parameter
        .map(|parameter| parameter.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or(&reference.id)
        .to_string()
}

/// The smallest and largest value a number type takes: what the product states, within what
/// its bits can hold.
pub(crate) fn number_limits(kind: &ParameterType, signed: bool) -> (i64, i64) {
    let bits = kind.size_in_bit.clamp(1, 63);
    let (low, high) = if signed {
        (-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
    } else {
        (0, (1i64 << bits) - 1)
    };
    (
        kind.minimum.map_or(low, |minimum| minimum.max(low)),
        kind.maximum.map_or(high, |maximum| maximum.min(high)),
    )
}

/// How many characters a text type holds.
pub(crate) fn text_length(program: &ApplicationProgram, kind: &ParameterType) -> usize {
    ((kind.size_in_bit / 8) as usize)
        .saturating_sub(usize::from(program.options.zero_terminate_text))
}

/// Check `value` for the parameter reference `reference` and return it in the form it is
/// stored in: the number without decoration, the enumeration member's own value.
///
/// A number must be whole and within the type's limits; an enumeration value must be one of
/// the type's members; a text must fit and consist of ISO 8859-1 characters. Parameters the
/// product does not let a user change, and types this crate does not encode, are refused.
pub fn check_value(
    program: &ApplicationProgram,
    reference: &str,
    value: &str,
) -> Result<String, EditError> {
    let (_, parameter_ref) = parameter_ref(program, reference)?;
    let (_, kind, access, _) = describe(program, parameter_ref)
        .ok_or_else(|| EditError::UnknownParameter(reference.to_string()))?;
    let name = label(program, parameter_ref);
    if access != Access::ReadWrite {
        return Err(EditError::NotEditable(format!("\"{name}\"")));
    }
    match &kind.kind {
        ParameterKind::Number { signed } => {
            let (low, high) = number_limits(kind, *signed);
            let number: i64 = value.trim().parse().map_err(|_| {
                EditError::Invalid(format!("enter a whole number from {low} to {high}"))
            })?;
            if number < low || number > high {
                return Err(EditError::Invalid(format!(
                    "{number} is outside {low} to {high}"
                )));
            }
            Ok(number.to_string())
        }
        ParameterKind::Restriction => kind
            .members
            .iter()
            .find(|member| member.value.trim() == value.trim())
            .map(|member| member.value.clone())
            .ok_or_else(|| EditError::Invalid(format!("{value:?} is not one of the choices"))),
        ParameterKind::Text => {
            let room = text_length(program, kind);
            let length = value.chars().count();
            if length > room {
                return Err(EditError::Invalid(format!(
                    "at most {room} characters fit, this has {length}"
                )));
            }
            if let Some(odd) = value.chars().find(|c| u32::from(*c) > 0xFF) {
                return Err(EditError::Invalid(format!(
                    "the character {odd:?} cannot be stored in the device"
                )));
            }
            Ok(value.to_string())
        }
        ParameterKind::None | ParameterKind::Binary(_) | ParameterKind::Other(_) => {
            Err(EditError::NotEditable(format!("\"{name}\"")))
        }
    }
}

/// The group addresses the project links to the group object reference `reference`.
pub fn project_links(
    program: &ApplicationProgram,
    baseline: &DeviceData,
    addresses: &GroupAddresses,
    reference: &str,
) -> Vec<GroupAddress> {
    let local = program.local_id(reference);
    baseline
        .object_links
        .iter()
        .find(|entry| program.local_id(&entry.reference) == local)
        .map(|entry| {
            entry
                .links
                .iter()
                .filter_map(|id| addresses.get(id))
                .collect()
        })
        .unwrap_or_default()
}

/// The identifier a group address gets in effective device data.
fn link_id(address: GroupAddress) -> String {
    format!("GA-x{}", address.raw())
}

impl Edits {
    /// Whether nothing is different from the project.
    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty() && self.links.is_empty()
    }

    /// Give the parameter reference `reference` the value `value`, after checking it with
    /// [`check_value`]. A value equal to the project's removes the edit.
    pub fn set_parameter(
        &mut self,
        program: &ApplicationProgram,
        baseline: &DeviceData,
        reference: &str,
        value: &str,
    ) -> Result<(), EditError> {
        let value = check_value(program, reference, value)?;
        let (at, _) = parameter_ref(program, reference)?;
        let local = program.local_id(reference).to_string();
        let project = Configuration::evaluate(program, baseline)?;
        if project.value(program, at)? == value {
            self.parameters.remove(&local);
        } else {
            self.parameters.insert(local, value);
        }
        Ok(())
    }

    /// Return the parameter reference `reference` to the project's value.
    pub fn reset_parameter(&mut self, program: &ApplicationProgram, reference: &str) {
        self.parameters.remove(program.local_id(reference));
    }

    /// Link the group object reference `reference` to `links`, the first being the sending
    /// address. Links equal to the project's remove the edit.
    ///
    /// Whether the addresses fit the product's tables shows when the download is prepared
    /// ([`crate::Download::prepare`]).
    pub fn set_links(
        &mut self,
        program: &ApplicationProgram,
        baseline: &DeviceData,
        addresses: &GroupAddresses,
        reference: &str,
        links: Vec<GroupAddress>,
    ) -> Result<(), EditError> {
        if program.com_object_ref_index(reference).is_none() {
            return Err(EditError::UnknownObject(reference.to_string()));
        }
        for (at, address) in links.iter().enumerate() {
            if links.iter().take(at).any(|earlier| earlier == address) {
                return Err(EditError::DuplicateLink(*address));
            }
        }
        let local = program.local_id(reference).to_string();
        if project_links(program, baseline, addresses, reference) == links {
            self.links.remove(&local);
        } else {
            self.links.insert(local, links);
        }
        Ok(())
    }

    /// The edits that lead from the configuration `base` gives to the one `self` gives,
    /// both being edits over the project's device data `project`: for every parameter and
    /// every group object where the two differ, what `self` has. Applied on top of what
    /// `base` leads to, they give what `self` leads to; [`changes`] puts them in words.
    ///
    /// This is what "changed" means once a device has been programmed with `base`: different
    /// from what the device holds, not from the project file. A value set back to the
    /// project's is no entry of `self` and still a difference to a `base` that has another.
    pub fn relative_to(
        &self,
        program: &ApplicationProgram,
        project: &DeviceData,
        addresses: &GroupAddresses,
        base: &Edits,
    ) -> Result<Edits, Error> {
        let configuration = Configuration::evaluate(program, project)?;
        let mut out = Edits::default();
        let references: std::collections::BTreeSet<&String> = self
            .parameters
            .keys()
            .chain(base.parameters.keys())
            .collect();
        for reference in references {
            let Ok((at, _)) = parameter_ref(program, reference) else {
                continue;
            };
            let projected = configuration.value(program, at)?;
            let value = |edits: &Edits| {
                edits
                    .parameters
                    .get(reference)
                    .map_or(projected, String::as_str)
                    .to_string()
            };
            if value(self) != value(base) {
                out.parameters.insert(reference.clone(), value(self));
            }
        }
        let references: std::collections::BTreeSet<&String> =
            self.links.keys().chain(base.links.keys()).collect();
        for reference in references {
            let links = |edits: &Edits| {
                edits
                    .links
                    .get(reference)
                    .cloned()
                    .unwrap_or_else(|| project_links(program, project, addresses, reference))
            };
            if links(self) != links(base) {
                out.links.insert(reference.clone(), links(self));
            }
        }
        Ok(out)
    }

    /// The device data a download is prepared from: the project's with these edits on top,
    /// and the group addresses it refers to (the project's and those the edits name).
    pub fn apply(
        &self,
        program: &ApplicationProgram,
        baseline: &DeviceData,
        addresses: &GroupAddresses,
    ) -> (DeviceData, GroupAddresses) {
        let mut data = baseline.clone();
        let mut known = addresses.clone();
        for (reference, value) in &self.parameters {
            let stored = data
                .parameter_values
                .iter_mut()
                .find(|entry| program.local_id(&entry.reference) == reference);
            match stored {
                Some(entry) => entry.value.clone_from(value),
                None => data.parameter_values.push(ParameterValue {
                    reference: reference.clone(),
                    value: value.clone(),
                }),
            }
        }
        for (reference, links) in &self.links {
            let ids: Vec<String> = links
                .iter()
                .map(|address| {
                    let id = link_id(*address);
                    known.insert(&id, *address);
                    id
                })
                .collect();
            let stored = data
                .object_links
                .iter_mut()
                .find(|entry| program.local_id(&entry.reference) == reference);
            match stored {
                Some(entry) => entry.links = ids,
                None => data.object_links.push(ObjectLinks {
                    reference: reference.clone(),
                    links: ids,
                    overrides: ObjectAttributes::default(),
                }),
            }
        }
        (data, known)
    }
}

/// How a value reads on a parameter page: the member's text for an enumeration, else the
/// value itself; with the parameter's suffix behind it.
pub fn display_value(
    program: &ApplicationProgram,
    reference: &ParameterRef,
    value: &str,
) -> String {
    let Some((_, kind, _, suffix)) = describe(program, reference) else {
        return value.to_string();
    };
    let text = match kind.kind {
        ParameterKind::Restriction => kind
            .members
            .iter()
            .find(|member| member.value.trim() == value.trim())
            .map_or(value, |member| member.text.as_str()),
        _ => value,
    };
    if suffix.is_empty() {
        text.to_string()
    } else {
        format!("{text} {suffix}")
    }
}

/// One parameter whose value differs from the baseline [`changes`] was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterChange {
    /// Identifier of the parameter reference.
    pub reference: String,
    /// Its label.
    pub label: String,
    /// The baseline's value, as it reads on a parameter page.
    pub from: String,
    /// The value now.
    pub to: String,
}

/// One group object whose links differ from the baseline [`changes`] was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkChange {
    /// Identifier of the group object reference.
    pub reference: String,
    /// Group object number.
    pub number: u16,
    /// What the object is called.
    pub name: String,
    /// Addresses linked now and not in the project.
    pub added: Vec<GroupAddress>,
    /// Addresses linked in the project and not now.
    pub removed: Vec<GroupAddress>,
    /// The links now, in order.
    pub now: Vec<GroupAddress>,
    /// Whether the sending address is another one.
    pub sending_changed: bool,
}

/// What differs between a device's effective configuration and the project.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changes {
    /// Parameters with another value, in the product's order.
    pub parameters: Vec<ParameterChange>,
    /// Group objects with other links, by object number.
    pub links: Vec<LinkChange>,
}

impl Changes {
    /// Whether nothing differs.
    pub fn is_empty(&self) -> bool {
        self.parameters.is_empty() && self.links.is_empty()
    }
}

/// Say in a person's terms what `edits` change against `baseline`: the project's device data,
/// or what a device was last programmed with (see [`Edits::relative_to`]).
pub fn changes(
    program: &ApplicationProgram,
    baseline: &DeviceData,
    addresses: &GroupAddresses,
    edits: &Edits,
) -> Result<Changes, Error> {
    let project = Configuration::evaluate(program, baseline)?;
    let mut out = Changes::default();
    for (at, reference) in program.parameter_refs.iter().enumerate() {
        let Some(value) = edits.parameters.get(&reference.id) else {
            continue;
        };
        out.parameters.push(ParameterChange {
            reference: reference.id.clone(),
            label: label(program, reference),
            from: display_value(program, reference, project.value(program, at)?),
            to: display_value(program, reference, value),
        });
    }
    for reference in &program.com_object_refs {
        let Some(now) = edits.links.get(&reference.id) else {
            continue;
        };
        let before = project_links(program, baseline, addresses, &reference.id);
        let object = program.com_object(&reference.object);
        out.links.push(LinkChange {
            reference: reference.id.clone(),
            number: object.map_or(0, |object| object.number),
            name: reference
                .text
                .clone()
                .or_else(|| object.map(|object| object.text.clone()))
                .unwrap_or_default(),
            added: now
                .iter()
                .filter(|address| !before.contains(address))
                .copied()
                .collect(),
            removed: before
                .iter()
                .filter(|address| !now.contains(address))
                .copied()
                .collect(),
            now: now.clone(),
            sending_changed: before.first() != now.first(),
        });
    }
    out.links.sort_by_key(|change| change.number);
    Ok(out)
}
