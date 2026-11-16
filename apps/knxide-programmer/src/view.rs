//! The JSON the page gets: the device list, the editor of one device, the group addresses
//! and the technical details.

use std::collections::HashMap;

use knxide_core::GroupAddress;
use knxide_program::compare::Finding;
use knxide_program::edit::Changes;
use knxide_program::product::ObjectPriority;
use knxide_program::view::{Control, DeviceView, Item, ObjectView, Standing};
use knxide_program::{
    Download, Obstacle, PlanOptions, Project, ProjectDevice, Request, SimulatedDevice,
};
use serde_json::{Value, json};

use crate::sidecar::State;

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn kind_name(download: &Download) -> &'static str {
    if download.kind.full {
        "full"
    } else {
        "partial"
    }
}

/// `1.1` for version byte `0x11`, as tools show application versions.
fn version_text(version: u8) -> String {
    format!("{}.{}", version >> 4, version & 0x0F)
}

/// Why the app does not download to a device that it lets you edit and compare: the product
/// asks for a manufacturer plug-in to carry out its download, and what that does on the bus is
/// not known. The README says more; the page gets one short sentence.
pub const DOWNLOAD_NOT_ENABLED: &str =
    "This product needs the manufacturer's download plug-in. Program it with ETS.";

/// What a device is, as far as its product's words tell: a word for the page to pick an icon
/// by, and what to call it. Guessed from the product text, the order number and the name of
/// the application; "other" when nothing fits.
pub fn kind(device: &ProjectDevice) -> (&'static str, &'static str) {
    let mut words = String::new();
    if let Some(product) = &device.product {
        words.push_str(&product.text);
        words.push(' ');
        words.push_str(&product.order_number);
        words.push(' ');
    }
    if let Some(program) = &device.application {
        words.push_str(&program.name);
    }
    let words = words.to_lowercase();
    let has = |any: &[&str]| any.iter().any(|word| words.contains(word));
    if has(&[
        "router",
        "coupler",
        "koppler",
        "ip interface",
        "ip-interface",
    ]) {
        ("net", "Router")
    } else if has(&[
        "push button",
        "push-button",
        "pushbutton",
        "tastsensor",
        "taster",
        "keypad",
    ]) {
        ("button", "Button panel")
    } else if has(&["dimm", "dim.", "variateur"]) {
        ("dimmer", "Dimmer")
    } else if has(&[
        "heating",
        "heiz",
        "valve",
        "thermostat",
        "temperature controller",
    ]) {
        ("heat", "Heating")
    } else if has(&["blind", "shutter", "jalousie", "rollladen", "volet"]) {
        ("blind", "Blind actuator")
    } else if has(&["presence", "motion", "detector", "sensor", "melder"]) {
        ("sensor", "Sensor")
    } else if has(&["switch", "schalt", "actuator", "aktor"]) {
        ("switch", "Switch actuator")
    } else {
        ("box", "Other")
    }
}

/// What stands in the way of a device, in one plain sentence: the reason it cannot be used
/// here at all, or, for a device that can be edited and compared, that its download is not
/// enabled. `None` for a device nothing stands in the way of.
pub fn reason(device: &ProjectDevice) -> Option<String> {
    if device.is_supported() {
        return None;
    }
    if device.is_editable() {
        return Some(DOWNLOAD_NOT_ENABLED.into());
    }
    // A download plug-in is the lesser trouble: name what else there is first.
    device
        .obstacles
        .iter()
        .find(|obstacle| **obstacle != Obstacle::DownloadPlugin)
        .or(device.obstacles.first())
        .map(plain_reason)
}

/// One obstacle in one short sentence, for someone who does not care how it is found. The
/// technical reading is the obstacle's own text.
pub fn plain_reason(obstacle: &Obstacle) -> String {
    match obstacle {
        Obstacle::DownloadPlugin => DOWNLOAD_NOT_ENABLED,
        Obstacle::UiPlugin => "Set up in the manufacturer's plug-in. Use ETS for this device.",
        Obstacle::UnsupportedMask(_) | Obstacle::ProcedureStyle => {
            "This kind of device is not handled here. Use ETS for it."
        }
        Obstacle::Secure => "Uses KNX Secure. Use ETS for this device.",
        Obstacle::AccessKey => "The project protects its devices with a key. Use ETS.",
        Obstacle::SecondApplication => "Has a second application program. Use ETS for this device.",
        Obstacle::NoAddress => "Has no address.",
        Obstacle::NoApplication => "Has no application program.",
        Obstacle::MissingApplication(_) => "Its application program is not in the project file.",
        _ => "This device cannot be handled here. Use ETS for it.",
    }
    .to_string()
}

/// A group address with what the project and the app know about it.
pub fn address_view(project: &Project, state: &State, address: GroupAddress) -> Value {
    let known = project.group_address(address);
    let created = state.created.get(&address);
    let name = state
        .renamed
        .get(&address)
        .cloned()
        .or_else(|| known.map(|entry| entry.name.clone()))
        .or_else(|| created.map(|entry| entry.name.clone()));
    let datapoint_type = known
        .and_then(|entry| entry.datapoint_type.clone())
        .or_else(|| created.and_then(|entry| entry.datapoint_type.clone()));
    json!({
        "address": address.to_string(),
        "raw": address.raw(),
        "name": name,
        "datapoint_type": datapoint_type,
    })
}

/// How a device stands against its baseline: what the app last programmed into it, or the
/// project where the app has never programmed it.
pub struct Standing2<'a> {
    /// `edited` while the device is waiting to be programmed, `downloaded` when the app
    /// knows what it holds, else `project`.
    pub state: &'a str,
    /// Number of parameters and group objects that differ from the baseline.
    pub edits: usize,
    /// When the app last downloaded the device.
    pub downloaded_ms: Option<u64>,
    /// Why the device is waiting when no single change says so: one short sentence.
    pub pending_note: Option<&'a str>,
}

/// One row of the device list.
pub fn device_row(device: &ProjectDevice, standing: &Standing2<'_>) -> Value {
    let application = device.application.as_ref().map(|program| {
        json!({
            "id": program.id,
            "name": program.name,
            "number": program.application_number,
            "version": version_text(program.application_version),
            "mask": format!("{:04X}", program.mask_version),
        })
    });
    json!({
        "id": device.id,
        "address": device.address.map(|address| address.to_string()),
        "sort": device.address.map(|address| address.raw()),
        "name": device.name,
        "location": device.location,
        "kind": kind(device).0,
        "type": kind(device).1,
        "order_number": device.product.as_ref().map(|product| product.order_number.clone()),
        "product": device.product.as_ref().map(|product| product.text.clone()),
        "application": application,
        // Three things a device may allow, each including the ones after it: download,
        // comparison, editing. A product that only asks for a download plug-in has the last
        // two.
        "supported": device.is_supported(),
        "comparable": device.is_editable(),
        "editable": device.is_editable(),
        "download_blocked": (device.is_editable() && !device.is_supported())
            .then_some(DOWNLOAD_NOT_ENABLED),
        "reason": reason(device),
        "obstacles": device
            .obstacles
            .iter()
            .map(|obstacle| json!({
                "code": obstacle.code(),
                "text": obstacle.to_string(),
                "plain": plain_reason(obstacle),
            }))
            .collect::<Vec<_>>(),
        "state": standing.state,
        "edits": standing.edits,
        "downloaded_ms": standing.downloaded_ms,
        "pending_note": standing.pending_note,
    })
}

fn flags(object: &ObjectView) -> Value {
    let on = |value: Option<bool>| value == Some(true);
    let attributes = &object.attributes;
    json!({
        "communication": on(attributes.communication),
        "read": on(attributes.read),
        "write": on(attributes.write),
        "transmit": on(attributes.transmit),
        "update": on(attributes.update),
        "read_on_init": on(attributes.read_on_init),
    })
}

/// One group object with its links, for the page.
pub fn object_view(project: &Project, state: &State, object: &ObjectView) -> Value {
    json!({
        "reference": object.reference,
        "number": object.number,
        "name": object.name,
        "function": object.function,
        "size_bits": object.attributes.size_in_bit,
        "datapoint_type": object.datapoint_type,
        "priority": match object.attributes.priority.unwrap_or(ObjectPriority::Low) {
            ObjectPriority::Low => "low",
            ObjectPriority::High => "high",
            ObjectPriority::Alert => "alert",
        },
        "flags": flags(object),
        "links": object
            .links
            .iter()
            .map(|address| address_view(project, state, *address))
            .collect::<Vec<_>>(),
    })
}

/// The parameter pages and group objects of a device, as the page renders them.
pub fn editor_view(project: &Project, state: &State, view: &DeviceView) -> Value {
    let pages: Vec<Value> = view
        .pages
        .iter()
        .map(|page| {
            let items: Vec<Value> = page
                .items
                .iter()
                .map(|item| match item {
                    Item::Text(text) => json!({ "type": "text", "text": text }),
                    Item::Parameter(parameter) => {
                        let control = match &parameter.control {
                            Control::Choice(choices) => json!({
                                "type": "choice",
                                "choices": choices
                                    .iter()
                                    .map(|(value, text)| json!({ "value": value, "text": text }))
                                    .collect::<Vec<_>>(),
                            }),
                            Control::Number { minimum, maximum } => json!({
                                "type": "number", "minimum": minimum, "maximum": maximum,
                            }),
                            Control::Text { max_length } => json!({
                                "type": "text", "max_length": max_length,
                            }),
                            Control::Fixed => json!({ "type": "fixed" }),
                        };
                        json!({
                            "type": "parameter",
                            "reference": parameter.reference,
                            "label": parameter.label,
                            "control": control,
                            "value": parameter.value,
                            "value_text": parameter.value_text,
                            "baseline_value": parameter.project_value,
                            "baseline_text": parameter.project_text,
                            "default_text": parameter.default_text,
                            "standing": match parameter.standing {
                                Standing::Default => "default",
                                Standing::Project => "project",
                                Standing::Edited => "edited",
                            },
                            "suffix": parameter.suffix,
                        })
                    }
                })
                .collect();
            json!({
                "id": page.id,
                "channel": page.channel,
                "title": page.title,
                "items": items,
            })
        })
        .collect();
    json!({
        "pages": pages,
        "objects": view
            .objects
            .iter()
            .map(|object| object_view(project, state, object))
            .collect::<Vec<_>>(),
        "unused": view
            .unused
            .iter()
            .map(|object| object_view(project, state, object))
            .collect::<Vec<_>>(),
    })
}

/// The page each parameter is on: the parameter's reference, and the page's title.
pub type PageTitles = HashMap<String, String>;

/// Which page each parameter of `view` is on. Several pages have parameters of the same
/// label ("Function" on every rocker's page), so a parameter is named with its page wherever
/// it stands alone.
pub fn page_titles(view: &DeviceView) -> PageTitles {
    let mut titles = PageTitles::new();
    for page in view.pages.iter().filter(|page| !page.title.is_empty()) {
        for item in &page.items {
            if let Item::Parameter(parameter) = item {
                titles.insert(parameter.reference.clone(), page.title.clone());
            }
        }
    }
    titles
}

/// A set of changes in words: against the baseline, or against the project file.
pub fn changes_view(
    project: &Project,
    state: &State,
    changes: &Changes,
    pages: &PageTitles,
) -> Value {
    let addresses = |list: &[GroupAddress]| -> Vec<Value> {
        list.iter()
            .map(|address| address_view(project, state, *address))
            .collect()
    };
    json!({
        "parameters": changes.parameters.iter().map(|change| json!({
            "reference": change.reference,
            "page": pages.get(&change.reference),
            "label": change.label,
            "from": change.from,
            "to": change.to,
        })).collect::<Vec<_>>(),
        "links": changes.links.iter().map(|change| json!({
            "reference": change.reference,
            "number": change.number,
            "name": change.name,
            "added": addresses(&change.added),
            "removed": addresses(&change.removed),
            "now": addresses(&change.now),
            "sending_changed": change.sending_changed,
        })).collect::<Vec<_>>(),
    })
}

/// What a comparison found, for a person.
pub fn findings_view(
    project: &Project,
    state: &State,
    findings: &[Finding],
    pages: &PageTitles,
) -> Vec<Value> {
    let addresses = |list: &[GroupAddress]| -> Vec<Value> {
        list.iter()
            .map(|address| address_view(project, state, *address))
            .collect()
    };
    findings
        .iter()
        .map(|finding| match finding {
            Finding::Parameter {
                reference,
                label,
                here_text,
                device_text,
                adoptable,
                ..
            } => json!({
                "type": "parameter",
                "reference": reference,
                "page": pages.get(reference),
                "label": label,
                "here": here_text,
                "device": device_text,
                "adoptable": adoptable,
            }),
            Finding::Links {
                reference,
                number,
                name,
                here,
                device,
            } => json!({
                "type": "links",
                "reference": reference,
                "number": number,
                "name": name,
                "here": addresses(here),
                "device": addresses(device),
                "adoptable": reference.is_some(),
            }),
            Finding::Bytes {
                address,
                here,
                device,
                what,
            } => json!({
                "type": "bytes",
                "address": format!("{address:04X}"),
                "here": hex(here),
                "device": hex(device),
                "what": what,
            }),
        })
        .collect()
}

/// How one request is shown: its line, and for writes whether it changes the device.
fn request_view(request: &Request) -> Value {
    let class = match request {
        Request::MemoryWrite { .. } | Request::PropertyWrite { .. } | Request::Restart => "write",
        Request::Connect | Request::Disconnect => "transport",
        _ => "read",
    };
    json!({ "text": request.to_string(), "class": class })
}

/// The requests `download` leads to against a model of a device that holds its application,
/// with the checks this server adds. `Err` carries the requests up to where the plan stops.
pub fn preview(
    download: &Download,
    options: &PlanOptions,
) -> Result<Vec<Request>, (Vec<Request>, String)> {
    let mut model = SimulatedDevice::holding(download);
    match download.plan(&mut model, options) {
        Ok(plan) => Ok(plan.requests),
        Err(failure) => Err((failure.requests, failure.error.to_string())),
    }
}

/// One variant of a download: what it sends and the requests against a modelled device.
fn variant_view(download: &Download, requests: &[Request], error: Option<String>) -> Value {
    let image = &download.image;
    let sent: Vec<String> = image
        .segments()
        .iter()
        .map(|segment| {
            segment
                .send
                .iter()
                .map(|send| if *send { '1' } else { '0' })
                .collect()
        })
        .collect();
    let bytes: usize = image
        .segments()
        .iter()
        .map(|segment| segment.send.iter().filter(|send| **send).count())
        .sum();
    json!({
        "kind": kind_name(download),
        "bytes_sent": bytes,
        "sent": sent,
        "writes": requests
            .iter()
            .filter(|request| {
                matches!(
                    request,
                    Request::MemoryWrite { .. } | Request::PropertyWrite { .. }
                )
            })
            .count(),
        "phases": download.phases(),
        "requests": requests.iter().map(request_view).collect::<Vec<_>>(),
        "error": error,
    })
}

/// The variants of a download: as prepared and, if that is a partial one, the full download
/// it turns into when the device does not hold the application.
pub fn variants(download: &Download, options: &PlanOptions) -> Vec<Value> {
    std::iter::once(download)
        .chain(download.fallback.as_deref())
        .map(|variant| match preview(variant, options) {
            Ok(requests) => variant_view(variant, &requests, None),
            Err((requests, error)) => variant_view(variant, &requests, Some(error)),
        })
        .collect()
}

/// What a download of the device sends, for those who want to see it: the communication
/// tables, the memory image and the requests.
pub fn technical(
    project: &Project,
    state: &State,
    device: &ProjectDevice,
    download: &Download,
    options: &PlanOptions,
) -> Value {
    let program = device.application.as_deref();
    let tables = download.tables.as_ref().map(|tables| {
        let addresses: Vec<Value> = tables
            .addresses
            .iter()
            .enumerate()
            .map(|(at, address)| {
                let mut entry = address_view(project, state, *address);
                entry["connection"] = json!(at + 1);
                entry
            })
            .collect();
        let associations: Vec<Value> = tables
            .associations
            .iter()
            .map(|association| {
                let address = usize::from(association.connection)
                    .checked_sub(1)
                    .and_then(|at| tables.addresses.get(at));
                json!({
                    "connection": association.connection,
                    "object": association.object,
                    "address": address.map(|address| address.to_string()),
                })
            })
            .collect();
        let objects: Vec<Value> = tables
            .objects
            .iter()
            .filter(|object| object.active)
            .map(|object| {
                let definition = program.and_then(|program| {
                    program
                        .com_objects
                        .iter()
                        .find(|candidate| candidate.number == object.number)
                });
                let on = |value: Option<bool>| value == Some(true);
                json!({
                    "number": object.number,
                    "name": definition.map(|definition| definition.text.clone()),
                    "function": definition.map(|definition| definition.function_text.clone()),
                    "linked": !object.links.is_empty(),
                    "size_bits": object.attributes.size_in_bit,
                    "flags": {
                        "communication": on(object.attributes.communication),
                        "read": on(object.attributes.read),
                        "write": on(object.attributes.write),
                        "transmit": on(object.attributes.transmit),
                        "update": on(object.attributes.update),
                        "read_on_init": on(object.attributes.read_on_init),
                    },
                    "links": object.links.iter().map(ToString::to_string).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({
            "address_table": format!("{:04X}", tables.address_table),
            "association_table": format!("{:04X}", tables.association_table),
            "object_table": format!("{:04X}", tables.object_table),
            "objects_total": tables.objects.len(),
            "addresses": addresses,
            "associations": associations,
            "objects": objects,
        })
    });
    let segments: Vec<Value> = download
        .image
        .segments()
        .iter()
        .map(|segment| json!({ "address": segment.address, "data": hex(&segment.data) }))
        .collect();
    json!({
        "mask": format!("{:04X}", download.profile.mask_version),
        "manufacturer": format!("{:04X}", download.application.manufacturer),
        "hardware": download.hardware.as_ref().map(|expected| hex(&expected.data)),
        "parameters_in_use": download.configuration.active_parameter_count(),
        "objects_in_use": download.configuration.active_object_count(),
        "tables": tables,
        "segments": segments,
        "variants": variants(download, options),
    })
}
