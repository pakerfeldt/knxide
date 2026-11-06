//! The MCP tools: their definitions and what they do.

use std::time::Duration;

use knxide_core::GroupAddress;
use knxide_core::Payload;
use knxide_core::dpt::{DptId, ValueKind};
use knxide_ip::tunnel::Transport;
use knxide_project::GroupAddressInfo;
use knxide_tokio::ConnectionState;
use serde_json::{Map, Value, json};

use crate::bus::{Bus, HISTORY, Link, Seen, now_ms};

const DEFAULT_LIMIT: usize = 50;
const MAX_FIND_LIMIT: usize = 500;
const DEFAULT_READ_TIMEOUT_MS: u64 = 3000;
const MAX_READ_TIMEOUT_MS: u64 = 30_000;

/// A tool failed; the message goes back to the model.
pub type ToolResult = Result<Value, String>;

/// Tool definitions for `tools/list`.
pub fn definitions(allow_write: bool) -> Vec<Value> {
    let read_only = json!({
        "readOnlyHint": true,
        "openWorldHint": true,
    });
    let mut tools = vec![
        json!({
            "name": "bus_status",
            "title": "Bus status",
            "description": "How the bus is reached (tunnelling or routing), the connection \
                state, the loaded ETS project, and whether writing is enabled. Does not \
                connect.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": read_only,
        }),
        json!({
            "name": "find_group_addresses",
            "title": "Find group addresses",
            "description": "Search the building's group addresses (from the ETS project) by \
                name, room, function, description, address or DPT. Every word in the query \
                must match. Returns each address with its datapoint type, the value format it \
                accepts, and the last value seen on the bus. Use this first to find the address \
                for a light, blind, temperature and so on.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Words to match, e.g. \"kitchen light\" or \"temperature\". \
                            Empty lists everything.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_FIND_LIMIT,
                        "description": format!("Most results to return. Default {DEFAULT_LIMIT}."),
                    },
                },
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        }),
        json!({
            "name": "read_group_address",
            "title": "Read group address",
            "description": "Ask the bus for the current value of a group address \
                (GroupValue_Read) and decode the answer with its datapoint type.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "address": address_schema(),
                    "dpt": dpt_schema(),
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 100,
                        "maximum": MAX_READ_TIMEOUT_MS,
                        "description": format!(
                            "How long to wait for an answer. Default {DEFAULT_READ_TIMEOUT_MS}."
                        ),
                    },
                },
                "required": ["address"],
            },
            "annotations": read_only,
        }),
        json!({
            "name": "recent_telegrams",
            "title": "Recent telegrams",
            "description": format!(
                "Group telegrams seen on the bus since the server connected (up to the last \
                 {HISTORY}), oldest first, with decoded values."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "address": {
                        "type": "string",
                        "description": "Only telegrams for this group address (e.g. \"1/2/3\") \
                            or exact group name.",
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": HISTORY,
                        "description": format!("Most telegrams to return. Default {DEFAULT_LIMIT}."),
                    },
                },
            },
            "annotations": read_only,
        }),
    ];
    if allow_write {
        tools.push(json!({
            "name": "write_group_address",
            "title": "Write group address",
            "description": "Send a value to a group address (GroupValue_Write). This acts on \
                real devices: lights, blinds, heating and so on. The value is text in the \
                format given by find_group_addresses (`accepts`), e.g. \"on\", \"21.5\", \
                \"comfort\". Without a known DPT the value is raw: 0-63 or hex like 0x0c1a.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "address": address_schema(),
                    "value": { "type": "string", "description": "The value to send." },
                    "dpt": dpt_schema(),
                },
                "required": ["address", "value"],
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true,
            },
        }));
    }
    tools
}

fn address_schema() -> Value {
    json!({
        "type": "string",
        "description": "Group address, e.g. \"1/2/3\", or the exact group name from the project.",
    })
}

fn dpt_schema() -> Value {
    json!({
        "type": "string",
        "description": "Datapoint type, e.g. \"1.001\" or \"9.001\". Defaults to the project's.",
    })
}

/// Run tool `name`. `Ok(None)` if there is no such tool.
pub async fn call(
    bus: &Bus,
    allow_write: bool,
    name: &str,
    args: &Map<String, Value>,
) -> Option<ToolResult> {
    Some(match name {
        "bus_status" => Ok(status(bus, allow_write)),
        "find_group_addresses" => find(bus, args),
        "read_group_address" => read(bus, args).await,
        "recent_telegrams" => recent(bus, args).await,
        "write_group_address" if allow_write => write(bus, args).await,
        _ => return None,
    })
}

fn status(bus: &Bus, allow_write: bool) -> Value {
    let (connection, address) = match bus.state() {
        None if bus.connecting() => ("connecting", None),
        None => ("not connected", None),
        Some(ConnectionState::Connecting) => ("reconnecting", None),
        Some(ConnectionState::Connected(a)) => ("connected", Some(a.to_string())),
        Some(ConnectionState::Closed) => ("closed", None),
    };
    let mut result = json!({
        "connection": connection,
        "individual_address": address,
        "project_file": bus.project_file(),
        "group_addresses": bus.project().len(),
        "write_enabled": allow_write,
    });
    let link = match bus.link() {
        None => json!({ "mode": "none" }),
        Some(Link::Tunnel { gateway, transport }) => json!({
            "mode": "tunnelling",
            "gateway": gateway,
            "transport": match transport {
                Transport::Tcp => "tcp",
                _ => "udp",
            },
        }),
        Some(Link::Routing(options)) => json!({
            "mode": "routing",
            "multicast_group": options.router.group.to_string(),
            "interface": options.interface.map(|i| i.to_string()),
        }),
    };
    if let (Some(obj), Value::Object(link)) = (result.as_object_mut(), link) {
        obj.extend(link);
    }
    result
}

fn find(bus: &Bus, args: &Map<String, Value>) -> ToolResult {
    let query = opt_str(args, "query")?.unwrap_or_default().to_lowercase();
    let limit = opt_limit(args, MAX_FIND_LIMIT)?;
    let terms: Vec<&str> = query.split_whitespace().collect();
    let matches: Vec<&GroupAddressInfo> = bus
        .project()
        .iter()
        .filter(|e| {
            let haystack = format!(
                "{} {} {} {} {}",
                e.address,
                e.name,
                e.description,
                e.path.join(" "),
                e.dpt.map(|d| d.to_string()).unwrap_or_default()
            )
            .to_lowercase();
            terms.iter().all(|t| haystack.contains(t))
        })
        .collect();
    let entries: Vec<Value> = matches
        .iter()
        .take(limit)
        .map(|e| {
            let mut entry = json!({
                "address": e.address.to_string(),
                "name": e.name,
                "path": e.path,
                "dpt": e.dpt.map(|d| d.to_string()),
            });
            if let Some(obj) = entry.as_object_mut() {
                if !e.description.is_empty() {
                    obj.insert("description".into(), json!(e.description));
                }
                if let Some(unit) = e.dpt.and_then(DptId::unit) {
                    obj.insert("unit".into(), json!(unit));
                }
                if let Some(accepts) = e.dpt.and_then(DptId::value_kind).map(accepts) {
                    obj.insert("accepts".into(), json!(accepts));
                }
                if let Some(seen) = bus.last(e.address) {
                    obj.insert("last_value".into(), value_of(e.dpt, &seen));
                    obj.insert("last_seen_s_ago".into(), json!(seconds_ago(&seen)));
                }
            }
            entry
        })
        .collect();
    let mut result = json!({
        "total": matches.len(),
        "returned": entries.len(),
        "group_addresses": entries,
    });
    if bus.project().is_empty()
        && let Some(obj) = result.as_object_mut()
    {
        obj.insert(
            "note".into(),
            json!("no ETS project loaded: start knxide-mcp with --project FILE"),
        );
    }
    Ok(result)
}

async fn read(bus: &Bus, args: &Map<String, Value>) -> ToolResult {
    let address = resolve(bus, req_str(args, "address")?)?;
    let dpt = dpt_for(bus, address, args)?;
    let timeout = match args.get("timeout_ms") {
        None | Some(Value::Null) => DEFAULT_READ_TIMEOUT_MS,
        Some(v) => v
            .as_u64()
            .filter(|t| (100..=MAX_READ_TIMEOUT_MS).contains(t))
            .ok_or(format!(
                "timeout_ms must be an integer from 100 to {MAX_READ_TIMEOUT_MS}"
            ))?,
    };
    let (source, payload) = bus.read(address, Duration::from_millis(timeout)).await?;
    let mut result = described(bus, address, dpt);
    if let Some(obj) = result.as_object_mut() {
        obj.insert("source".into(), json!(source.to_string()));
        obj.insert("raw".into(), json!(payload.to_string()));
        obj.insert("value".into(), decode(dpt, &payload));
    }
    Ok(result)
}

async fn recent(bus: &Bus, args: &Map<String, Value>) -> ToolResult {
    let address = opt_str(args, "address")?
        .map(|a| resolve(bus, a))
        .transpose()?;
    let limit = opt_limit(args, HISTORY)?;
    let telegrams: Vec<Value> = bus
        .recent(address, limit)
        .iter()
        .map(|seen| {
            let entry = bus.project().get(seen.address);
            let dpt = entry.and_then(|e| e.dpt);
            let mut t = json!({
                "s_ago": seconds_ago(seen),
                "source": seen.source.to_string(),
                "address": seen.address.to_string(),
                "service": seen.service,
            });
            if let Some(obj) = t.as_object_mut() {
                if let Some(e) = entry.filter(|e| !e.name.is_empty()) {
                    obj.insert("name".into(), json!(e.name));
                }
                if seen.payload.is_some() {
                    obj.insert("value".into(), value_of(dpt, seen));
                }
            }
            t
        })
        .collect();
    let empty = telegrams.is_empty();
    let mut result = json!({ "telegrams": telegrams });
    if empty
        && !matches!(bus.state(), Some(ConnectionState::Connected(_)))
        && let Some(obj) = result.as_object_mut()
    {
        obj.insert(
            "note".into(),
            json!("not connected to the bus yet; telegrams are recorded once connected"),
        );
    }
    Ok(result)
}

async fn write(bus: &Bus, args: &Map<String, Value>) -> ToolResult {
    let address = resolve(bus, req_str(args, "address")?)?;
    let text = req_str(args, "value")?;
    let dpt = dpt_for(bus, address, args)?;
    let (payload, shown) = match dpt {
        Some(dpt) => {
            let value = dpt.parse_value(text).map_err(|e| match dpt.value_kind() {
                Some(kind) => format!(
                    "{text:?} is not a valid {dpt} value ({e}); accepts {}",
                    accepts(kind)
                ),
                None => format!("{text:?} is not a valid {dpt} value: {e}"),
            })?;
            let payload = dpt
                .encode(&value)
                .map_err(|e| format!("cannot encode {text:?} as {dpt}: {e}"))?;
            (payload, json!(dpt.format(&value).to_string()))
        }
        None => {
            let payload = Payload::parse_raw(text).ok_or(format!(
                "{address} has no known DPT, so the value must be raw (0-63 or hex like 0x0c1a), \
                 or pass dpt"
            ))?;
            (payload, Value::Null)
        }
    };
    bus.write(address, payload.clone()).await?;
    let mut result = described(bus, address, dpt);
    if let Some(obj) = result.as_object_mut() {
        obj.insert("written".into(), shown);
        obj.insert("raw".into(), json!(payload.to_string()));
    }
    Ok(result)
}

/// A group address as text or an exact group name.
fn resolve(bus: &Bus, text: &str) -> Result<GroupAddress, String> {
    let text = text.trim();
    if let Ok(address) = text.parse::<GroupAddress>() {
        return Ok(address);
    }
    let named: Vec<&GroupAddressInfo> = bus
        .project()
        .iter()
        .filter(|e| e.name.trim().eq_ignore_ascii_case(text))
        .collect();
    match named.as_slice() {
        [one] => Ok(one.address),
        [] => Err(format!(
            "{text:?} is neither a group address nor a group name; use find_group_addresses"
        )),
        many => Err(format!(
            "{text:?} names {} group addresses ({}); pass the address instead",
            many.len(),
            many.iter()
                .map(|e| e.address.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The DPT from the `dpt` argument, else the project.
fn dpt_for(
    bus: &Bus,
    address: GroupAddress,
    args: &Map<String, Value>,
) -> Result<Option<DptId>, String> {
    match opt_str(args, "dpt")?
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        Some(text) => {
            let dpt: DptId = text.parse().map_err(|_| format!("{text:?} is not a DPT"))?;
            if !dpt.is_supported() {
                return Err(format!("DPT {dpt} is not supported"));
            }
            Ok(Some(dpt))
        }
        None => Ok(bus.dpt(address).filter(|d| d.is_supported())),
    }
}

/// Address, name and DPT as the start of a result.
fn described(bus: &Bus, address: GroupAddress, dpt: Option<DptId>) -> Value {
    let name = bus
        .project()
        .get(address)
        .map(|e| e.name.clone())
        .filter(|n| !n.is_empty());
    json!({
        "address": address.to_string(),
        "name": name,
        "dpt": dpt.map(|d| d.to_string()),
    })
}

fn decode(dpt: Option<DptId>, payload: &Payload) -> Value {
    dpt.and_then(|dpt| dpt.decode(payload).ok().map(|v| dpt.format(&v).to_string()))
        .map_or(Value::Null, Value::String)
}

/// The decoded value of a telegram, or its raw payload if it cannot be decoded.
fn value_of(dpt: Option<DptId>, seen: &Seen) -> Value {
    let Some(payload) = &seen.payload else {
        return Value::Null;
    };
    match decode(dpt, payload) {
        Value::Null => json!(format!("raw {payload}")),
        decoded => decoded,
    }
}

fn seconds_ago(seen: &Seen) -> u64 {
    now_ms().saturating_sub(seen.time_ms) / 1000
}

/// The text [`DptId::parse_value`] accepts for a kind of value.
fn accepts(kind: ValueKind) -> String {
    match kind {
        ValueKind::Bool { off, on } => format!("{off} or {on}"),
        ValueKind::Control => "control,value as 0/1 each, e.g. 1,0".into(),
        ValueKind::Step => "step +1..+7 (increase), -1..-7 (decrease), 0 to stop".into(),
        ValueKind::Integer { min, max } => format!("whole number {min} to {max}"),
        ValueKind::Float { min, max } if min == f32::MIN && max == f32::MAX => "number".into(),
        ValueKind::Float { min, max } => format!("number {min} to {max}"),
        ValueKind::TimeOfDay => "time [Mon ]HH:MM[:SS]".into(),
        ValueKind::Date => "date YYYY-MM-DD".into(),
        ValueKind::Text { max_len } => format!("text, at most {max_len} characters"),
        ValueKind::SceneControl => "scene number 1-64 to activate, or \"learn N\"".into(),
        ValueKind::HvacMode => "auto, comfort, standby, economy or building-protection".into(),
        ValueKind::Rgb => "colour #RRGGBB or r,g,b".into(),
    }
}

fn req_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    opt_str(args, key)?.ok_or(format!("missing argument {key:?}"))
}

fn opt_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("argument {key:?} must be a string")),
    }
}

fn opt_limit(args: &Map<String, Value>, max: usize) -> Result<usize, String> {
    match args.get("limit") {
        None | Some(Value::Null) => Ok(DEFAULT_LIMIT.min(max)),
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=max).contains(n))
            .ok_or(format!("limit must be an integer from 1 to {max}")),
    }
}
