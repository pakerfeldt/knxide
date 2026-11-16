//! What a person sees of a device: the parameter pages that are visible with the current
//! values, each parameter with its control, and the group objects in use.
//!
//! Visibility is worked out here, with the same rules as for the download
//! ([`Configuration`]): a page or a parameter is listed when the walk through the product's
//! structure reaches it and the product lets users see it.

use knxide_core::GroupAddress;

use crate::config::{Configuration, test_matches};
use crate::device::{DeviceData, GroupAddresses};
use crate::edit::{describe, display_value, number_limits, text_length};
use crate::error::Error;
use crate::product::{Access, ApplicationProgram, DynamicNode, ObjectAttributes, ParameterKind};

/// How a parameter is edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// One of a list: (value, text) in the product's order.
    Choice(Vec<(String, String)>),
    /// A whole number within limits.
    Number {
        /// Smallest value.
        minimum: i64,
        /// Largest value.
        maximum: i64,
    },
    /// Text of limited length.
    Text {
        /// Most characters that fit.
        max_length: usize,
    },
    /// Shown, not edited: the product does not let users change it, or its type is not one
    /// this crate encodes.
    Fixed,
}

/// Where a parameter's value stands relative to the project and the product.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// The product's default.
    Default,
    /// Another value than the default, as the project has it.
    Project,
    /// Another value than the project has.
    Edited,
}

/// One parameter on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterView {
    /// Identifier of the parameter reference.
    pub reference: String,
    /// Its label.
    pub label: String,
    /// How it is edited.
    pub control: Control,
    /// The value now.
    pub value: String,
    /// The value as it reads: the member's text for a choice.
    pub value_text: String,
    /// The project's value, as it reads.
    pub project_text: String,
    /// The project's value.
    pub project_value: String,
    /// The product's default, as it reads.
    pub default_text: String,
    /// Where the value stands.
    pub standing: Standing,
    /// What follows the value, a unit for instance; may be empty.
    pub suffix: String,
}

/// One thing on a parameter page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A parameter.
    Parameter(ParameterView),
    /// A line of text: a separator, a hint, a heading.
    Text(String),
}

/// A parameter page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// Identifier, stable while the product stays the same.
    pub id: String,
    /// The channel the page belongs to; may be empty.
    pub channel: String,
    /// Its title.
    pub title: String,
    /// What it shows, in order.
    pub items: Vec<Item>,
}

/// One group object in use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectView {
    /// Identifier of the group object reference links are stored under.
    pub reference: String,
    /// Group object number.
    pub number: u16,
    /// What it is called.
    pub name: String,
    /// What it does.
    pub function: String,
    /// Size, flags and priority.
    pub attributes: ObjectAttributes,
    /// The datapoint types the product names; may be empty.
    pub datapoint_type: String,
    /// The linked group addresses; the first is the sending one.
    pub links: Vec<GroupAddress>,
}

/// What a person sees of a device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DeviceView {
    /// The visible parameter pages, in the product's order.
    pub pages: Vec<Page>,
    /// The group objects in use, by number.
    pub objects: Vec<ObjectView>,
    /// Group objects that have links but are not in use with the current values.
    pub unused: Vec<ObjectView>,
}

struct Walker<'a> {
    program: &'a ApplicationProgram,
    project: &'a Configuration,
    effective: &'a Configuration,
    pages: Vec<Page>,
    /// For each page, where its title stands in the product's display order, if stated.
    orders: Vec<Option<u32>>,
    /// Whether the walk is inside a block that is not shown.
    hidden: bool,
    /// Parameters a person may see that sit in a block that is not shown, with where they
    /// stand in the display order. They are put on pages afterwards.
    astray: Vec<(u32, Item)>,
}

impl Walker<'_> {
    /// The page items go to: `current`, or a page of the channel's own for parameters that sit
    /// outside every page.
    fn page(&mut self, current: Option<usize>, channel: &str) -> Option<&mut Page> {
        match current {
            Some(at) => self.pages.get_mut(at),
            None => {
                let id = format!("channel:{channel}");
                if !self.pages.iter().any(|page| page.id == id) {
                    self.pages.push(Page {
                        id: id.clone(),
                        channel: channel.to_string(),
                        title: if channel.is_empty() {
                            "General".to_string()
                        } else {
                            channel.to_string()
                        },
                        items: Vec::new(),
                    });
                    self.orders.push(None);
                }
                self.pages.iter_mut().find(|page| page.id == id)
            }
        }
    }

    fn walk(
        &mut self,
        nodes: &[DynamicNode],
        channel: &str,
        current: Option<usize>,
    ) -> Result<(), Error> {
        for node in nodes {
            match node {
                DynamicNode::ParameterRef(id) if self.hidden => {
                    // In a block that is not shown. A parameter a person may see still has
                    // its place in the display order, and with it a page: see `view`.
                    let order = self
                        .program
                        .parameter_ref_index(id)
                        .and_then(|at| self.program.parameter_refs.get(at))
                        .and_then(|reference| reference.display_order);
                    if let Some(order) = order
                        && let Some(item) = self.parameter(id)?
                    {
                        self.astray.push((order, item));
                    }
                }
                DynamicNode::ParameterRef(id) => {
                    if let Some(item) = self.parameter(id)?
                        && let Some(page) = self.page(current, channel)
                    {
                        page.items.push(item);
                    }
                }
                DynamicNode::ComObjectRef(_) => {}
                DynamicNode::Separator(text) => {
                    if !text.trim().is_empty()
                        && !self.hidden
                        && let Some(page) = self.page(current, channel)
                    {
                        page.items.push(Item::Text(text.clone()));
                    }
                }
                DynamicNode::Choose {
                    parameter_ref,
                    branches,
                } => {
                    let at = self
                        .program
                        .parameter_ref_index(parameter_ref)
                        .ok_or_else(|| Error::UnknownReference(parameter_ref.clone()))?;
                    let value = self.effective.value(self.program, at)?.to_string();
                    let mut matched = false;
                    for branch in branches.iter().filter(|branch| !branch.default) {
                        if test_matches(branch.test.as_deref().unwrap_or_default(), &value) {
                            matched = true;
                            self.walk(&branch.children, channel, current)?;
                        }
                    }
                    if !matched {
                        for branch in branches.iter().filter(|branch| branch.default) {
                            self.walk(&branch.children, channel, current)?;
                        }
                    }
                }
                DynamicNode::Channel { text, children } => self.walk(children, text, None)?,
                DynamicNode::Block {
                    id,
                    text,
                    title,
                    access,
                    children,
                } => {
                    let was_hidden = self.hidden;
                    if *access == Access::None {
                        // The block is no page. What it holds still counts: see above.
                        self.hidden = true;
                        self.walk(children, channel, current)?;
                        self.hidden = was_hidden;
                        continue;
                    }
                    let title_ref = title.as_deref().and_then(|reference| {
                        let at = self.program.parameter_ref_index(reference)?;
                        self.program.parameter_refs.get(at)
                    });
                    let titled = title_ref
                        .and_then(|reference| {
                            describe(self.program, reference).map(|(label, ..)| label)
                        })
                        .filter(|label| !label.trim().is_empty())
                        .unwrap_or(text);
                    let at = self.pages.len();
                    self.pages.push(Page {
                        id: if id.is_empty() {
                            format!("page:{at}")
                        } else {
                            id.clone()
                        },
                        channel: channel.to_string(),
                        title: titled.trim().to_string(),
                        items: Vec::new(),
                    });
                    self.orders
                        .push(title_ref.and_then(|reference| reference.display_order));
                    self.hidden = false;
                    self.walk(children, channel, Some(at))?;
                    self.hidden = was_hidden;
                }
                DynamicNode::Group(children) => self.walk(children, channel, current)?,
            }
        }
        Ok(())
    }

    fn parameter(&self, id: &str) -> Result<Option<Item>, Error> {
        let program = self.program;
        let at = program
            .parameter_ref_index(id)
            .ok_or_else(|| Error::UnknownReference(id.to_string()))?;
        let Some(reference) = program.parameter_refs.get(at) else {
            return Ok(None);
        };
        let Some((label, kind, access, suffix)) = describe(program, reference) else {
            return Ok(None);
        };
        if access == Access::None {
            return Ok(None);
        }
        if kind.kind == ParameterKind::None {
            // A parameter without a value is a line of text.
            return Ok((!label.trim().is_empty()).then(|| Item::Text(label.to_string())));
        }
        let editable = access == Access::ReadWrite;
        let control = match &kind.kind {
            ParameterKind::Restriction if editable => Control::Choice(
                kind.members
                    .iter()
                    .map(|member| (member.value.clone(), member.text.clone()))
                    .collect(),
            ),
            ParameterKind::Number { signed } if editable => {
                let (minimum, maximum) = number_limits(kind, *signed);
                Control::Number { minimum, maximum }
            }
            ParameterKind::Text if editable => Control::Text {
                max_length: text_length(program, kind),
            },
            _ => Control::Fixed,
        };
        let value = self.effective.value(program, at)?;
        let project_value = self.project.value(program, at)?;
        let default = match &reference.value {
            Some(value) => value.as_str(),
            None => program
                .parameter(&reference.parameter)
                .map_or("", |parameter| parameter.value.as_str()),
        };
        let standing = if value != project_value {
            Standing::Edited
        } else if value != default {
            Standing::Project
        } else {
            Standing::Default
        };
        // The suffix is shown beside the control, so the readings here are without it.
        let reading = |value: &str| {
            let full = display_value(program, reference, value);
            match full.strip_suffix(suffix) {
                Some(bare) if !suffix.is_empty() => bare.trim_end().to_string(),
                _ => full,
            }
        };
        Ok(Some(Item::Parameter(ParameterView {
            reference: reference.id.clone(),
            label: crate::edit::label(program, reference),
            control,
            value_text: reading(value),
            project_text: reading(project_value),
            default_text: reading(default),
            value: value.to_string(),
            project_value: project_value.to_string(),
            standing,
            suffix: suffix.to_string(),
        })))
    }
}

/// What a person sees of a device of `program`: `effective` is its data now, `baseline` what
/// the project stores (they are the same for a device nobody has edited). `addresses` resolves
/// the links of `effective`.
pub fn view(
    program: &ApplicationProgram,
    baseline: &DeviceData,
    effective: &DeviceData,
    addresses: &GroupAddresses,
) -> Result<DeviceView, Error> {
    let project = Configuration::evaluate(program, baseline)?;
    let now = Configuration::evaluate(program, effective)?;
    let mut walker = Walker {
        program,
        project: &project,
        effective: &now,
        pages: Vec::new(),
        orders: Vec::new(),
        hidden: false,
        astray: Vec::new(),
    };
    walker.walk(&program.dynamic, "", None)?;
    // Products made for older tools keep parameters a person may change in blocks that are
    // not shown, and leave the pages they belong on empty. What ties the two together is the
    // display order: a page's title has a place in it, and what follows up to the next page
    // belongs on that page. Inferred from product data; unverified against the tool.
    walker.astray.sort_by_key(|(order, _)| *order);
    for (order, item) in std::mem::take(&mut walker.astray) {
        let page = walker
            .orders
            .iter()
            .enumerate()
            .filter_map(|(at, page_order)| Some((at, (*page_order)?)))
            .filter(|(_, page_order)| *page_order <= order)
            .max_by_key(|(_, page_order)| *page_order)
            .and_then(|(at, _)| walker.pages.get_mut(at));
        if let Some(page) = page {
            page.items.push(item);
        }
    }
    let mut pages = walker.pages;
    pages.retain(|page| !page.items.is_empty());

    // One row per group object: the reference the device links, else the first one in use.
    let links_of = |reference: &str| -> Option<Vec<GroupAddress>> {
        effective
            .object_links
            .iter()
            .find(|entry| program.local_id(&entry.reference) == reference)
            .map(|entry| {
                entry
                    .links
                    .iter()
                    .filter_map(|id| addresses.get(id))
                    .collect()
            })
    };
    let mut objects: Vec<ObjectView> = Vec::new();
    let mut unused = Vec::new();
    for (at, reference) in program.com_object_refs.iter().enumerate() {
        let Some(object) = program.com_object(&reference.object) else {
            continue;
        };
        let links = links_of(&reference.id);
        let row = ObjectView {
            reference: reference.id.clone(),
            number: object.number,
            name: reference
                .text
                .clone()
                .unwrap_or_else(|| object.text.clone()),
            function: reference
                .function_text
                .clone()
                .unwrap_or_else(|| object.function_text.clone()),
            attributes: reference.overrides.or(object.attributes),
            datapoint_type: reference
                .datapoint_type
                .clone()
                .unwrap_or_else(|| object.datapoint_type.clone()),
            links: links.clone().unwrap_or_default(),
        };
        if !now.object_active(at) {
            if !row.links.is_empty() {
                unused.push(row);
            }
            continue;
        }
        match objects.iter_mut().find(|other| other.number == row.number) {
            // The same object through another reference: the linked one is the one to show.
            Some(other) if other.links.is_empty() && !row.links.is_empty() => *other = row,
            Some(_) => {}
            None => objects.push(row),
        }
    }
    objects.sort_by_key(|object| object.number);
    unused.sort_by_key(|object: &ObjectView| object.number);
    Ok(DeviceView {
        pages,
        objects,
        unused,
    })
}
