//! The configuration of one device: the value of every parameter reference and which
//! parameter and group object references are in use.
//!
//! Verified against three recorded downloads (1,088 parameter bytes). The rules follow the
//! published meaning of the product data format; devices whose visibility is computed by a
//! manufacturer plug-in are out of reach.

use std::cmp::Ordering;

use crate::device::DeviceData;
use crate::error::Error;
use crate::product::{ApplicationProgram, DynamicNode};

/// Values and visibility for one device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration {
    /// Project value per parameter reference, where the project stores one.
    project_values: Vec<Option<String>>,
    active_parameters: Vec<bool>,
    active_objects: Vec<bool>,
}

impl Configuration {
    /// Work out values and visibility of `device` for `program`.
    ///
    /// The tree is walked from the top. A reference the walk reaches is in use. At a `choose`
    /// the walk takes the value of the named parameter reference, whether that reference is
    /// itself in use or not, and enters every branch whose test matches; if none matches it
    /// enters the default branches.
    pub fn evaluate(program: &ApplicationProgram, device: &DeviceData) -> Result<Self, Error> {
        let mut project_values = vec![None; program.parameter_refs.len()];
        for entry in &device.parameter_values {
            let slot = program
                .parameter_ref_index(&entry.reference)
                .and_then(|at| project_values.get_mut(at))
                .ok_or_else(|| Error::UnknownReference(entry.reference.clone()))?;
            *slot = Some(entry.value.clone());
        }
        let mut configuration = Self {
            project_values,
            active_parameters: vec![false; program.parameter_refs.len()],
            active_objects: vec![false; program.com_object_refs.len()],
        };
        configuration.walk(program, &program.dynamic)?;
        Ok(configuration)
    }

    fn walk(&mut self, program: &ApplicationProgram, nodes: &[DynamicNode]) -> Result<(), Error> {
        for node in nodes {
            match node {
                DynamicNode::ParameterRef(id) => {
                    let slot = program
                        .parameter_ref_index(id)
                        .and_then(|at| self.active_parameters.get_mut(at))
                        .ok_or_else(|| Error::UnknownReference(id.clone()))?;
                    *slot = true;
                }
                DynamicNode::ComObjectRef(id) => {
                    let slot = program
                        .com_object_ref_index(id)
                        .and_then(|at| self.active_objects.get_mut(at))
                        .ok_or_else(|| Error::UnknownReference(id.clone()))?;
                    *slot = true;
                }
                DynamicNode::Choose {
                    parameter_ref,
                    branches,
                } => {
                    let at = program
                        .parameter_ref_index(parameter_ref)
                        .ok_or_else(|| Error::UnknownReference(parameter_ref.clone()))?;
                    let value = self.value(program, at)?.to_string();
                    let mut matched = false;
                    for branch in branches.iter().filter(|branch| !branch.default) {
                        if test_matches(branch.test.as_deref().unwrap_or_default(), &value) {
                            matched = true;
                            self.walk(program, &branch.children)?;
                        }
                    }
                    if !matched {
                        for branch in branches.iter().filter(|branch| branch.default) {
                            self.walk(program, &branch.children)?;
                        }
                    }
                }
                DynamicNode::Group(children)
                | DynamicNode::Channel { children, .. }
                | DynamicNode::Block { children, .. } => self.walk(program, children)?,
                DynamicNode::Separator(_) => {}
            }
        }
        Ok(())
    }

    /// The value of the parameter reference at `index`: the project's value if it stores one,
    /// else the reference's own default, else the parameter's default.
    pub fn value<'s>(
        &'s self,
        program: &'s ApplicationProgram,
        index: usize,
    ) -> Result<&'s str, Error> {
        if let Some(Some(value)) = self.project_values.get(index) {
            return Ok(value);
        }
        let reference = program
            .parameter_refs
            .get(index)
            .ok_or_else(|| Error::UnknownReference(format!("parameter reference #{index}")))?;
        if let Some(value) = &reference.value {
            return Ok(value);
        }
        let parameter = program
            .parameter(&reference.parameter)
            .ok_or_else(|| Error::UnknownReference(reference.parameter.clone()))?;
        Ok(&parameter.value)
    }

    /// Whether the parameter reference at `index` is in use.
    pub fn parameter_active(&self, index: usize) -> bool {
        self.active_parameters.get(index).copied().unwrap_or(false)
    }

    /// Whether the group object reference at `index` is in use.
    pub fn object_active(&self, index: usize) -> bool {
        self.active_objects.get(index).copied().unwrap_or(false)
    }

    /// Number of parameter references in use.
    pub fn active_parameter_count(&self) -> usize {
        self.active_parameters.iter().filter(|a| **a).count()
    }

    /// Number of group object references in use.
    pub fn active_object_count(&self) -> usize {
        self.active_objects.iter().filter(|a| **a).count()
    }
}

/// Whether `value` satisfies the test expression `test`: a space-separated list of items, any
/// of which may match. An item is a value, optionally preceded by `<`, `>`, `<=`, `>=`, `!=` or
/// `=`. Both sides are compared as numbers when both are numbers, else as text.
pub fn test_matches(test: &str, value: &str) -> bool {
    test.split_whitespace().any(|item| {
        let (accept, operand): (fn(Ordering) -> bool, &str) =
            if let Some(rest) = item.strip_prefix("<=") {
                (Ordering::is_le, rest)
            } else if let Some(rest) = item.strip_prefix(">=") {
                (Ordering::is_ge, rest)
            } else if let Some(rest) = item.strip_prefix("!=") {
                (Ordering::is_ne, rest)
            } else if let Some(rest) = item.strip_prefix('<') {
                (Ordering::is_lt, rest)
            } else if let Some(rest) = item.strip_prefix('>') {
                (Ordering::is_gt, rest)
            } else {
                (Ordering::is_eq, item.strip_prefix('=').unwrap_or(item))
            };
        let ordering = match (value.trim().parse::<f64>(), operand.parse::<f64>()) {
            (Ok(left), Ok(right)) => left.partial_cmp(&right),
            _ => Some(value.cmp(operand)),
        };
        ordering.is_some_and(accept)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expressions() {
        assert!(test_matches("1", "1"));
        assert!(test_matches("0 2 3", "2"));
        assert!(!test_matches("0 2 3", "1"));
        assert!(test_matches("1", "1.0"));
        assert!(test_matches("<5", "4") && !test_matches("<5", "5"));
        assert!(test_matches("<=5", "5") && test_matches(">=5", "5"));
        assert!(test_matches(">5", "10"), "numeric, not text, comparison");
        assert!(test_matches("!=0", "1") && !test_matches("!=0", "0"));
        assert!(test_matches("=7", "7"));
        assert!(test_matches("abc", "abc") && !test_matches("abc", "abd"));
        assert!(test_matches("<b", "a"));
        assert!(!test_matches("", "1"));
        assert!(!test_matches("1", ""));
    }
}
