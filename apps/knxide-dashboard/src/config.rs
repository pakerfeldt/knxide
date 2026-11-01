//! The dashboard layout: pages of tiles bound to group addresses. Stored as JSON.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use knxide_core::GroupAddress;
use knxide_project::{GroupAddressInfo, GroupAddressTable};
use serde::{Deserialize, Serialize};

/// The whole dashboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dashboard {
    pub title: String,
    /// Language of the page's own texts: `en` or `sv`.
    #[serde(default = "default_language")]
    pub language: String,
    pub pages: Vec<Page>,
}

fn default_language() -> String {
    "en".into()
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            title: "Home".into(),
            language: default_language(),
            pages: vec![Page {
                id: "home".into(),
                name: "Home".into(),
                tiles: Vec::new(),
            }],
        }
    }
}

/// One page, usually a floor or a wing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub tiles: Vec<Tile>,
}

/// A tile. Group addresses are strings as typed (`1/2/3`).
///
/// In JSON the kind's fields sit next to the common ones: `{"type": "switch", "id": …,
/// "label": …, "group": …, "ga": …, "status": …}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tile {
    pub id: String,
    pub label: String,
    /// Room or section the tile is shown under. Tiles without one come last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(flatten)]
    pub kind: Kind,
}

/// What a tile does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    /// Shows a value.
    Value { ga: String },
    /// On/off, with an optional separate status address.
    Switch {
        ga: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    /// On/off plus brightness (DPT 5.001).
    Dimmer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        switch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        switch_status: Option<String>,
        brightness: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brightness_status: Option<String>,
    },
    /// Activates a scene (DPT 17.001 or 18.001).
    Scene { ga: String, scene: u8 },
    /// Room climate: target with − and +, the measured temperature and the heating output.
    Setpoint {
        ga: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actual: Option<String>,
        /// Heating output in percent (DPT 5.001).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        heating: Option<String>,
        #[serde(default = "default_step")]
        step: f32,
    },
    /// Blinds or awnings: up/down (1.008 or 1.001), stop, position (5.001).
    Blinds {
        #[serde(default, rename = "move", skip_serializing_if = "Option::is_none")]
        move_ga: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position_status: Option<String>,
    },
    /// HVAC operating mode (DPT 20.102).
    Mode {
        ga: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    /// Sends a fixed value.
    Button {
        ga: String,
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dpt: Option<String>,
    },
}

fn default_step() -> f32 {
    0.5
}

impl Tile {
    pub fn new(label: impl Into<String>, kind: Kind) -> Self {
        Self {
            id: String::new(),
            label: label.into(),
            group: None,
            kind,
        }
    }

    /// Every group address the tile uses.
    pub fn addresses(&self) -> Vec<&str> {
        let all: Vec<Option<&String>> = match &self.kind {
            Kind::Value { ga } | Kind::Scene { ga, .. } | Kind::Button { ga, .. } => vec![Some(ga)],
            Kind::Switch { ga, status } | Kind::Mode { ga, status } => {
                vec![Some(ga), status.as_ref()]
            }
            Kind::Dimmer {
                switch,
                switch_status,
                brightness,
                brightness_status,
            } => {
                vec![
                    switch.as_ref(),
                    switch_status.as_ref(),
                    Some(brightness),
                    brightness_status.as_ref(),
                ]
            }
            Kind::Setpoint {
                ga,
                status,
                actual,
                heating,
                ..
            } => {
                vec![Some(ga), status.as_ref(), actual.as_ref(), heating.as_ref()]
            }
            Kind::Blinds {
                move_ga,
                stop,
                position,
                position_status,
            } => {
                vec![
                    move_ga.as_ref(),
                    stop.as_ref(),
                    position.as_ref(),
                    position_status.as_ref(),
                ]
            }
        };
        all.into_iter()
            .flatten()
            .map(String::as_str)
            .filter(|s| !s.trim().is_empty())
            .collect()
    }

    /// Addresses whose value the tile shows, to read at start-up.
    pub fn shown(&self) -> Vec<&str> {
        let shown: Vec<Option<&String>> = match &self.kind {
            Kind::Value { ga } => vec![Some(ga)],
            Kind::Switch { ga, status } | Kind::Mode { ga, status } => {
                vec![Some(status.as_ref().unwrap_or(ga))]
            }
            Kind::Dimmer {
                switch,
                switch_status,
                brightness,
                brightness_status,
            } => {
                vec![
                    switch_status.as_ref().or(switch.as_ref()),
                    Some(brightness_status.as_ref().unwrap_or(brightness)),
                ]
            }
            Kind::Setpoint {
                ga,
                status,
                actual,
                heating,
                ..
            } => {
                vec![
                    Some(status.as_ref().unwrap_or(ga)),
                    actual.as_ref(),
                    heating.as_ref(),
                ]
            }
            Kind::Blinds {
                position,
                position_status,
                ..
            } => vec![position_status.as_ref().or(position.as_ref())],
            Kind::Scene { .. } | Kind::Button { .. } => vec![],
        };
        shown.into_iter().flatten().map(String::as_str).collect()
    }
}

impl Dashboard {
    /// Check every address parses. Returns a message naming the first bad one.
    pub fn validate(&self) -> Result<(), String> {
        if self.pages.is_empty() {
            return Err("the dashboard needs at least one page".into());
        }
        for page in &self.pages {
            for tile in &page.tiles {
                for ga in tile.addresses() {
                    ga.trim().parse::<GroupAddress>().map_err(|_| {
                        format!("{ga:?} on page {:?} is not a group address", page.name)
                    })?;
                }
            }
        }
        Ok(())
    }

    /// Group addresses to read so tiles have values.
    pub fn shown_addresses(&self) -> BTreeSet<GroupAddress> {
        self.pages
            .iter()
            .flat_map(|p| &p.tiles)
            .flat_map(Tile::shown)
            .filter_map(|ga| ga.trim().parse().ok())
            .collect()
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let dashboard: Self =
                    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                dashboard
                    .validate()
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                Ok(dashboard)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(format!("{}: {err}", path.display())),
        }
    }

    /// Write atomically: a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json + "\n")?;
        std::fs::rename(tmp, path)
    }
}

// ---------------------------------------------------------------------------
// Suggestions from the ETS project
// ---------------------------------------------------------------------------

/// What a group address does, guessed from its DPT and name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Switch,
    SwitchStatus,
    Brightness,
    BrightnessStatus,
    Setpoint,
    SetpointStatus,
    Heating,
    Move,
    Stop,
    Position,
    PositionStatus,
    Scene,
    Mode,
    Value,
    Skip,
}

/// Name endings, in several languages, that mark a role. Longest first, so "status värde"
/// wins over "status".
const SUFFIXES: &[(&str, Role)] = &[
    ("faktiskt börvärde", Role::SetpointStatus),
    ("börvärde status", Role::SetpointStatus),
    ("bör status", Role::SetpointStatus),
    ("faktiskt bör", Role::SetpointStatus),
    ("actual setpoint", Role::SetpointStatus),
    ("börvärde", Role::Setpoint),
    ("bör", Role::Setpoint),
    ("setpoint", Role::Setpoint),
    ("sollwert", Role::Setpoint),
    ("värmestatus", Role::Heating),
    ("heating output", Role::Heating),
    ("stellgröße", Role::Heating),
    ("status ljusstyrka", Role::BrightnessStatus),
    ("status värde", Role::BrightnessStatus),
    ("brightness status", Role::BrightnessStatus),
    ("rückmeldung helligkeit", Role::BrightnessStatus),
    ("status position", Role::PositionStatus),
    ("position status", Role::PositionStatus),
    ("dim absolut", Role::Brightness),
    ("helligkeit", Role::Brightness),
    ("brightness", Role::Brightness),
    ("ljusstyrka", Role::Brightness),
    ("dimvärde", Role::Brightness),
    ("rückmeldung", Role::SwitchStatus),
    ("status", Role::SwitchStatus),
    ("state", Role::SwitchStatus),
    ("bryt", Role::Switch),
    ("switch", Role::Switch),
    ("schalten", Role::Switch),
    ("position", Role::Position),
    ("stopp", Role::Stop),
    ("stop", Role::Stop),
    ("dimmen", Role::Skip),
    ("dim", Role::Skip),
];

const SETPOINT_WORDS: &[&str] = &["börvärde", "setpoint", "sollwert", "target"];

/// Leading words that mark the current (actual) value of a setpoint: "Faktiskt börvärde X".
const ACTUAL_PREFIXES: &[&str] = &["faktiskt ", "actual ", "ist-"];

/// Split a name into (base, role from its ending or an "actual" prefix).
fn split_name(name: &str) -> (String, Option<Role>) {
    let lower = name.to_lowercase();
    for prefix in ACTUAL_PREFIXES {
        if lower.starts_with(prefix) {
            let rest: String = name.chars().skip(prefix.chars().count()).collect();
            return (rest.trim().to_string(), Some(Role::SetpointStatus));
        }
    }
    for (suffix, role) in SUFFIXES {
        if let Some(base) = lower.strip_suffix(suffix)
            && (base.is_empty() || base.ends_with(' '))
        {
            let cut = name.chars().count() - suffix.chars().count();
            let base: String = name.chars().take(cut).collect();
            return (base.trim().to_string(), Some(*role));
        }
    }
    (name.trim().to_string(), None)
}

fn role(entry: &GroupAddressInfo, suffix_role: Option<Role>) -> Role {
    let Some(dpt) = entry.dpt else {
        return Role::Skip;
    };
    let lower = entry.name.to_lowercase();
    let status = matches!(
        suffix_role,
        Some(Role::SwitchStatus | Role::BrightnessStatus | Role::PositionStatus)
    );
    match (dpt.main, dpt.sub) {
        (1, Some(8)) => Role::Move,
        (1, Some(7 | 17)) => Role::Stop,
        (1, _) if suffix_role == Some(Role::Stop) => Role::Stop,
        (1, _) if status => Role::SwitchStatus,
        (1, _) => Role::Switch,
        (3, _) => Role::Skip,
        (5, _) if suffix_role == Some(Role::Heating) => Role::Heating,
        (5, Some(1)) if suffix_role == Some(Role::Position) => Role::Position,
        (5, Some(1)) if suffix_role == Some(Role::PositionStatus) => Role::PositionStatus,
        (5, Some(1)) if suffix_role == Some(Role::BrightnessStatus) => Role::BrightnessStatus,
        (5, Some(1)) if suffix_role == Some(Role::Brightness) => Role::Brightness,
        // A percentage without a brightness ending (valve position, humidity) is a reading.
        (5, _) => Role::Value,
        (9, _) if suffix_role == Some(Role::SetpointStatus) => Role::SetpointStatus,
        (9, _) if suffix_role == Some(Role::Setpoint) => Role::Setpoint,
        (9, Some(1)) if SETPOINT_WORDS.iter().any(|w| lower.contains(w)) => {
            if status {
                Role::SetpointStatus
            } else {
                Role::Setpoint
            }
        }
        (17 | 18, _) => Role::Scene,
        (20, Some(102)) => Role::Mode,
        _ => Role::Value,
    }
}

/// Group-range paths in the project (main, and main › middle), for choosing what to suggest.
pub fn ranges(project: &GroupAddressTable) -> Vec<Vec<String>> {
    let mut set = BTreeSet::new();
    for e in project.iter() {
        for depth in 1..=e.path.len() {
            set.insert(
                e.path
                    .get(..depth)
                    .map(<[String]>::to_vec)
                    .unwrap_or_default(),
            );
        }
    }
    set.into_iter().collect()
}

/// The room a label starts with: its first word, or two when the second is a number
/// ("Rum 2 eluttag" → "Rum 2").
pub fn room_of(label: &str) -> Option<String> {
    let mut words = label.split_whitespace();
    let first = words.next()?;
    match words.next() {
        Some(second) if second.chars().all(|c| c.is_ascii_digit()) => {
            Some(format!("{first} {second}"))
        }
        _ => Some(first.to_string()),
    }
}

/// Put tiles that share a room word (at least two of them) into that room.
pub fn assign_groups(tiles: &mut [Tile]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for tile in tiles.iter() {
        if let Some(room) = room_of(&tile.label) {
            *counts.entry(room.to_lowercase()).or_default() += 1;
        }
    }
    for tile in tiles.iter_mut() {
        if tile.group.is_some() {
            continue;
        }
        tile.group = room_of(&tile.label)
            .filter(|room| counts.get(&room.to_lowercase()).copied().unwrap_or(0) >= 2);
    }
}

/// Propose tiles for the group addresses under `path`, pairing commands with their status
/// addresses by name ("Kök tak bryt" + "Kök tak status" become one switch tile, and
/// "Hall" + "Hall bör" + "Hall faktiskt bör" + "Hall värmestatus" one climate tile),
/// and sorting them into rooms by their first word.
pub fn suggest(project: &GroupAddressTable, path: &[String]) -> Vec<Tile> {
    /// Addresses sharing a base name.
    struct Group {
        label: String,
        /// Role, address, and whether it is a temperature (DPT 9.001).
        roles: Vec<(Role, String, bool)>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for entry in project.iter().filter(|e| e.path.starts_with(path)) {
        let (base, suffix_role) = split_name(&entry.name);
        let role = role(entry, suffix_role);
        if role == Role::Skip {
            continue;
        }
        let key = base.to_lowercase();
        let ga = entry.address.to_string();
        let temperature = entry.dpt.is_some_and(|d| d.main == 9 && d.sub == Some(1));
        match groups.iter_mut().find(|g| g.label.to_lowercase() == key) {
            Some(group) => group.roles.push((role, ga, temperature)),
            None => groups.push(Group {
                label: base,
                roles: vec![(role, ga, temperature)],
            }),
        }
    }

    let name_of = |ga: &str, fallback: &str| {
        ga.parse()
            .ok()
            .and_then(|ga| project.get(ga))
            .map_or_else(|| fallback.to_string(), |e| e.name.clone())
    };
    let mut tiles = Vec::new();
    for group in groups {
        let find = |r: Role| {
            group
                .roles
                .iter()
                .find(|(role, ..)| *role == r)
                .map(|(_, ga, _)| ga.clone())
        };
        let label = group.label.clone();
        // Readings left over once the main tile has taken what it uses.
        let mut used: Vec<String> = Vec::new();
        if let Some(brightness) = find(Role::Brightness) {
            tiles.push(Tile::new(
                label,
                Kind::Dimmer {
                    switch: find(Role::Switch),
                    switch_status: find(Role::SwitchStatus),
                    brightness,
                    brightness_status: find(Role::BrightnessStatus),
                },
            ));
            continue;
        } else if find(Role::Move).is_some() || find(Role::Position).is_some() {
            // Awnings often use a plain on/off (1.001) address for up/down.
            let move_ga = find(Role::Move).or_else(|| find(Role::Switch));
            tiles.push(Tile::new(
                label,
                Kind::Blinds {
                    move_ga,
                    stop: find(Role::Stop),
                    position: find(Role::Position),
                    position_status: find(Role::PositionStatus),
                },
            ));
            continue;
        } else if let Some(ga) = find(Role::Setpoint) {
            // The room's own temperature reading, if it sits next to the setpoint.
            let actual = group
                .roles
                .iter()
                .find(|(r, _, temp)| *r == Role::Value && *temp)
                .map(|(_, ga, _)| ga.clone());
            let heating = find(Role::Heating);
            used.extend(actual.clone());
            used.extend(heating.clone());
            tiles.push(Tile::new(
                label,
                Kind::Setpoint {
                    ga,
                    status: find(Role::SetpointStatus),
                    actual,
                    heating,
                    step: 0.5,
                },
            ));
        } else if let Some(ga) = find(Role::Switch) {
            tiles.push(Tile::new(
                label,
                Kind::Switch {
                    ga,
                    status: find(Role::SwitchStatus),
                },
            ));
            continue;
        } else if let Some(ga) = find(Role::Mode) {
            tiles.push(Tile::new(label, Kind::Mode { ga, status: None }));
            continue;
        } else if let Some(ga) = find(Role::Scene) {
            tiles.push(Tile::new(label, Kind::Scene { ga, scene: 1 }));
            continue;
        }
        // Status-only addresses and readings become displays.
        for (role, ga, _) in &group.roles {
            let reading = matches!(
                role,
                Role::Value
                    | Role::Heating
                    | Role::SwitchStatus
                    | Role::BrightnessStatus
                    | Role::PositionStatus
            ) || (*role == Role::SetpointStatus && find(Role::Setpoint).is_none());
            if reading && !used.contains(ga) {
                tiles.push(Tile::new(
                    name_of(ga, &group.label),
                    Kind::Value { ga: ga.clone() },
                ));
            }
        }
    }
    for (n, tile) in tiles.iter_mut().enumerate() {
        tile.id = format!("s{}", n + 1);
    }
    assign_groups(&mut tiles);
    tiles
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = r#"<GroupAddress-Export>
      <GroupRange Name="Belysning"><GroupRange Name="Plan 1">
        <GroupAddress Name="Hall tak bryt" Address="1/0/10" DPTs="DPST-1-1" />
        <GroupAddress Name="Hall tak dim" Address="1/0/11" DPTs="DPST-3-7" />
        <GroupAddress Name="Hall tak status" Address="1/0/12" DPTs="DPST-1-1" />
        <GroupAddress Name="Hall tak status värde" Address="1/0/13" DPTs="DPST-5-1" />
        <GroupAddress Name="Hall tak dim absolut" Address="1/0/14" DPTs="DPST-5-1" />
        <GroupAddress Name="Hall eluttag" Address="1/0/20" DPTs="DPST-1-1" />
        <GroupAddress Name="Hall eluttag status" Address="1/0/22" DPTs="DPST-1-1" />
      </GroupRange></GroupRange>
      <GroupRange Name="Temperatur"><GroupRange Name="Rum">
        <GroupAddress Name="Hall" Address="2/0/10" DPTs="DPST-9-1" />
        <GroupAddress Name="Hall bör" Address="2/0/11" DPTs="DPST-9-1" />
        <GroupAddress Name="Hall värmestatus" Address="2/0/12" DPTs="DPST-5-1" />
        <GroupAddress Name="Hall faktiskt bör" Address="2/0/13" DPTs="DPST-9-1" />
        <GroupAddress Name="Börvärde ventilation" Address="2/0/20" DPTs="DPST-9-1" />
        <GroupAddress Name="Faktiskt börvärde ventilation" Address="2/0/21" DPTs="DPST-9-1" />
        <GroupAddress Name="Förråd" Address="2/0/30" DPTs="DPST-9-1" />
        <GroupAddress Name="Förråd bör" Address="2/0/31" DPTs="DPST-9-1" />
        <GroupAddress Name="Förråd bör status" Address="2/0/32" DPTs="DPST-9-1" />
        <GroupAddress Name="Markis altan" Address="3/0/1" DPTs="DPST-1-1" />
        <GroupAddress Name="Markis altan stop" Address="3/0/2" DPTs="DPST-1-1" />
        <GroupAddress Name="Markis altan position" Address="3/0/3" DPTs="DPST-5-1" />
      </GroupRange></GroupRange>
    </GroupAddress-Export>"#;

    #[test]
    fn suggestions_pair_commands_and_status() {
        let project = GroupAddressTable::parse(PROJECT).unwrap();
        let tiles = suggest(&project, &["Belysning".into(), "Plan 1".into()]);
        assert_eq!(tiles.len(), 2, "{tiles:#?}");
        assert_eq!(tiles[0].label, "Hall tak");
        assert!(
            matches!(&tiles[0].kind, Kind::Dimmer { switch: Some(s), switch_status: Some(ss), brightness, brightness_status: Some(bs) }
            if s == "1/0/10" && ss == "1/0/12" && brightness == "1/0/14" && bs == "1/0/13")
        );
        assert!(
            matches!(&tiles[1].kind, Kind::Switch { ga, status: Some(st) } if ga == "1/0/20" && st == "1/0/22")
        );
        // Both start with "Hall": they share a room.
        assert_eq!(tiles[0].group.as_deref(), Some("Hall"));
        assert_eq!(tiles[1].group.as_deref(), Some("Hall"));

        let temps = suggest(&project, &["Temperatur".into()]);
        assert_eq!(temps.len(), 4, "{temps:#?}");
        // "Hall", "Hall bör", "Hall faktiskt bör", "Hall värmestatus": one climate tile.
        assert_eq!(temps[0].label, "Hall");
        assert!(
            matches!(&temps[0].kind, Kind::Setpoint { ga, status: Some(st), actual: Some(act), heating: Some(heat), .. }
            if ga == "2/0/11" && st == "2/0/13" && act == "2/0/10" && heat == "2/0/12")
        );
        // "Faktiskt börvärde ventilation" is the status of "Börvärde ventilation".
        assert!(
            matches!(&temps[1].kind, Kind::Setpoint { ga, status: Some(st), .. } if ga == "2/0/20" && st == "2/0/21")
        );
        assert!(
            matches!(&temps[2].kind, Kind::Setpoint { ga, status: Some(st), actual: Some(act), .. }
            if ga == "2/0/31" && st == "2/0/32" && act == "2/0/30")
        );
        assert!(
            matches!(&temps[3].kind, Kind::Blinds { move_ga: Some(m), stop: Some(st), position: Some(p), .. }
            if m == "3/0/1" && st == "3/0/2" && p == "3/0/3")
        );
        // A room word used only once is no room.
        assert_eq!(temps[1].group, None);
        assert_eq!(ranges(&project).len(), 4);
    }

    #[test]
    fn rooms() {
        assert_eq!(room_of("Hall 2 eluttag").as_deref(), Some("Hall 2"));
        assert_eq!(room_of("Kök takbelysning").as_deref(), Some("Kök"));
        assert_eq!(room_of("").as_deref(), None);
    }

    #[test]
    fn json_shape_and_validation() {
        let mut dashboard = Dashboard::default();
        dashboard.pages[0].tiles = suggest(&GroupAddressTable::parse(PROJECT).unwrap(), &[]);
        let json = serde_json::to_string(&dashboard).unwrap();
        assert!(json.contains(r#""type":"dimmer""#), "{json}");
        let back: Dashboard = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dashboard);
        assert!(back.validate().is_ok());
        assert!(back.shown_addresses().contains(&"1/0/12".parse().unwrap()));

        // Files written before rooms existed still load.
        let old = r#"{"title":"Hem","pages":[{"id":"p","name":"P","tiles":[
            {"type":"switch","id":"a","label":"L","ga":"1/0/65","status":"1/0/67"},
            {"type":"setpoint","id":"b","label":"S","ga":"2/0/11","status":null,"actual":null,"step":0.5},
            {"type":"blinds","id":"c","label":"B","move":"3/0/1","stop":null}]}]}"#;
        let old: Dashboard = serde_json::from_str(old).unwrap();
        assert_eq!(old.language, "en");
        assert!(
            matches!(&old.pages[0].tiles[0].kind, Kind::Switch { status: Some(s), .. } if s == "1/0/67")
        );
        assert!(
            matches!(&old.pages[0].tiles[2].kind, Kind::Blinds { move_ga: Some(m), .. } if m == "3/0/1")
        );

        let mut bad = back.clone();
        bad.pages[0].tiles.push(Tile::new(
            "x",
            Kind::Value {
                ga: "99/9/9".into(),
            },
        ));
        assert!(bad.validate().is_err());
    }
}
