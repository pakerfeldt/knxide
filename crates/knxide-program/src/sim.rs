//! A stand-in device for planning without a bus: it answers from a recording where it has
//! one and from a small model of a well-behaved device otherwise.
//!
//! The model keeps memory, property values and load states. A load event moves its state
//! machine to the state the event asks for; a memory write is echoed while the verify-mode bit
//! of the device control property is set; unknown properties are reported as absent.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;

use crate::download::Download;
use crate::mask::{LoadControl, MaskProfile, Resource};
use crate::plan::{Device, Expect, Fault};
use crate::procedure::{Action, Target};
use crate::request::{Request, Response};
use crate::wire::answers;

/// A recording does not pair up: a response has no request it could belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingError {
    /// Zero-based position of the response that does not fit.
    pub response: usize,
}

impl fmt::Display for RecordingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "recorded response #{} does not answer any request before it",
            self.response + 1
        )
    }
}

impl std::error::Error for RecordingError {}

/// A replayed or modelled device.
#[derive(Debug, Clone)]
pub struct SimulatedDevice {
    mask_version: u16,
    profile: Option<MaskProfile>,
    /// Recorded answers per request, in recording order. `None` records "no answer".
    script: HashMap<Request, VecDeque<Option<Response>>>,
    memory: BTreeMap<u16, u8>,
    /// Property data by object, property and start element.
    properties: HashMap<(u8, u8, u16), Vec<u8>>,
    load_states: HashMap<u8, u8>,
    /// Whether memory reads are answered with no data: see [`Self::refuse_memory_reads`].
    memory_refused: bool,
    scripted: usize,
    modelled: usize,
}

/// Index of the load state machine of the application program.
const APPLICATION_STATE_MACHINE: u8 = 3;
/// Index of the load state machine of the second application program.
const SECOND_APPLICATION_STATE_MACHINE: u8 = 4;

/// Whether `request` can go unanswered in a healthy exchange.
fn may_go_unanswered(request: &Request) -> bool {
    matches!(
        request,
        Request::Connect | Request::Disconnect | Request::Restart | Request::MemoryWrite { .. }
    )
}

impl SimulatedDevice {
    /// A device of the given mask version that answers from the model alone. Masks this crate
    /// has no profile for get the generic parts of the model.
    pub fn new(mask_version: u16) -> Self {
        let profile = MaskProfile::for_mask(mask_version).ok();
        let mut properties = HashMap::new();
        if profile.is_some_and(|profile| profile.verify_mode) {
            // Device control, verify mode off.
            properties.insert((0, 14, 1), vec![0x00]);
        }
        Self {
            mask_version,
            profile,
            script: HashMap::new(),
            memory: BTreeMap::new(),
            properties,
            load_states: HashMap::new(),
            memory_refused: false,
            scripted: 0,
            modelled: 0,
        }
    }

    /// A device on which `download` is expected to go through: it reports the application's
    /// manufacturer and identity, a loaded application, the hardware identification the image
    /// expects and the values the procedure's comparison steps look for. Its memory is empty.
    ///
    /// This is what a plan is previewed against when there is no device to ask; a real device
    /// answers differently wherever its memory already holds what the download writes.
    pub fn holding(download: &Download) -> Self {
        let profile = download.profile;
        let mut device = Self::new(profile.mask_version);
        let set = |device: &mut Self, resource: Option<Resource>, data: &[u8]| match resource {
            Some(Resource::Property { object, property }) => {
                device.set_property(object, property, 1, data);
            }
            Some(Resource::Memory(address)) => device.set_memory(address, data),
            None => {}
        };
        set(&mut device, profile.serial_number, &[0; 6]);
        set(
            &mut device,
            profile.manufacturer_id,
            &download.application.manufacturer.to_be_bytes(),
        );
        set(
            &mut device,
            profile.application_id,
            &download.application.to_bytes(),
        );
        if let Some(check) = profile.hardware {
            let data = match &download.hardware {
                Some(expected) => expected.data.clone(),
                None => vec![0; usize::from(check.length)],
            };
            set(&mut device, Some(check.read), &data);
        }
        // The tables and the application are loaded; a second application is not.
        for object in 1..=APPLICATION_STATE_MACHINE {
            device.load_states.insert(object, 1);
        }
        // What comparison steps expect and image reads ask for, one entry per request frame
        // at APDU length 15.
        let variants = std::iter::once(download).chain(download.fallback.as_deref());
        for step in variants.flat_map(|variant| &variant.procedure) {
            let (object, property, start, count, expected) = match &step.action {
                Action::CompareProperty {
                    object: Target::Index(object),
                    property,
                    start,
                    count,
                    inline,
                    invert: false,
                    ..
                } => (*object, *property, *start, *count, inline.as_deref()),
                Action::LoadImageProperty {
                    object: Target::Index(object),
                    property,
                    start,
                    count,
                } => (*object, *property, *start, *count, None),
                _ => continue,
            };
            let (Some(size), Ok(property)) = (
                profile.property_element_size(object, property),
                u8::try_from(property),
            ) else {
                continue;
            };
            let per_frame = (10 / size).clamp(1, 15);
            let zeros = vec![0u8; usize::from(count) * size];
            let data = expected
                .and_then(|data| data.get(..zeros.len()))
                .unwrap_or(&zeros);
            for (frame, chunk) in (0u16..).zip(data.chunks(per_frame * size)) {
                let first = start.saturating_add(frame.saturating_mul(per_frame as u16));
                device.set_property(object, property, first, chunk);
            }
        }
        device
    }

    /// A device that replays a recorded exchange. Requests and responses are given in the order
    /// they were recorded; each response is matched to the request it follows. A request the
    /// recording does not contain (or contains fewer times than it is asked) is answered from
    /// the model.
    pub fn from_recording(
        mask_version: u16,
        requests: &[Request],
        responses: &[Response],
    ) -> Result<Self, RecordingError> {
        let mut device = Self::new(mask_version);
        device.replay(requests, responses)?;
        Ok(device)
    }

    /// Add a recorded exchange to this device: its answers take precedence over the model's,
    /// each one once. See [`SimulatedDevice::from_recording`].
    pub fn replay(
        &mut self,
        requests: &[Request],
        responses: &[Response],
    ) -> Result<(), RecordingError> {
        let mut next = 0;
        // A memory write is answered only while verify mode is on; without this, a write
        // followed by a read of the same bytes could not be told from an echoed write.
        let mut echo = false;
        for request in requests {
            match request {
                Request::PropertyWrite {
                    object: 0,
                    property: 14,
                    data,
                    ..
                } => echo = data.first().is_some_and(|control| control & 0x04 != 0),
                Request::Restart => echo = false,
                _ => {}
            }
            let answer = responses.get(next).filter(|response| {
                answers(request, response)
                    && (echo || !matches!(request, Request::MemoryWrite { .. }))
            });
            if answer.is_some() {
                next += 1;
            } else if !may_go_unanswered(request) {
                return Err(RecordingError { response: next });
            }
            self.script
                .entry(request.clone())
                .or_default()
                .push_back(answer.cloned());
        }
        if next < responses.len() {
            return Err(RecordingError { response: next });
        }
        Ok(())
    }

    /// Give property `property` of object `object` a value: `data` is what a read starting at
    /// element `start` returns.
    pub fn set_property(&mut self, object: u8, property: u8, start: u16, data: &[u8]) {
        self.properties
            .insert((object, property, start), data.to_vec());
    }

    /// Put the load state machine with index `object` (1 address table, 2 association table,
    /// 3 application) into `state`: 0 unloaded, 1 loaded, 2 loading.
    pub fn set_load_state(&mut self, object: u8, state: u8) {
        self.load_states.insert(object, state);
    }

    /// Answer memory reads with no data from now on (or no longer), as a device does that
    /// keeps its memory to itself: one of mask 0021 was seen to, with its application
    /// unloaded. Load states that are read through memory are still given.
    pub fn refuse_memory_reads(&mut self, refuse: bool) {
        self.memory_refused = refuse;
    }

    /// Set memory content starting at `address`.
    pub fn set_memory(&mut self, address: u16, data: &[u8]) {
        for (offset, byte) in data.iter().enumerate() {
            self.memory
                .insert(address.wrapping_add(offset as u16), *byte);
        }
    }

    /// The modelled content of `address`; memory never written reads as zero.
    pub fn memory(&self, address: u16) -> u8 {
        self.memory.get(&address).copied().unwrap_or(0)
    }

    /// Number of requests answered from the recording so far.
    pub fn scripted(&self) -> usize {
        self.scripted
    }

    /// Number of requests answered from the model so far.
    pub fn modelled(&self) -> usize {
        self.modelled
    }

    fn echoes_writes(&self) -> bool {
        self.properties
            .get(&(0, 14, 1))
            .and_then(|data| data.first())
            .is_some_and(|control| control & 0x04 != 0)
    }

    /// The load state machine whose state is readable at memory address `at`, if any.
    fn state_machine_at(&self, at: u16) -> Option<u8> {
        let profile = self.profile?;
        if let LoadControl::Memory { status, .. } = profile.load_control
            && (status..status.saturating_add(4)).contains(&at)
        {
            return Some((at - status + 1) as u8);
        }
        if profile.application_load_status == Some(Resource::Memory(at)) {
            return Some(APPLICATION_STATE_MACHINE);
        }
        if profile.second_application_load_status == Some(Resource::Memory(at)) {
            return Some(SECOND_APPLICATION_STATE_MACHINE);
        }
        None
    }

    /// Apply a load event and return the new state.
    fn load_event(&mut self, object: u8, event: u8) -> u8 {
        let state = self.load_states.entry(object).or_insert(0);
        *state = match event {
            1 | 3 => 2,
            2 => 1,
            4 => 0,
            _ => *state,
        };
        *state
    }

    /// The model's answer; also moves the model's state along.
    fn model(&mut self, request: &Request) -> Option<Response> {
        let memory_control = match self.profile.map(|profile| profile.load_control) {
            Some(LoadControl::Memory { control, status }) => Some((control, status)),
            _ => None,
        };
        match request {
            Request::Connect | Request::Disconnect | Request::Restart => None,
            Request::DeviceDescriptorRead => Some(Response::DeviceDescriptor {
                mask_version: self.mask_version,
            }),
            Request::Authorize { .. } => Some(Response::Authorize { level: 0 }),
            Request::PropertyRead {
                object,
                property,
                start,
                count,
            } => {
                let data = if *property == 5 && memory_control.is_none() {
                    self.load_states.get(object).map(|state| vec![*state])
                } else {
                    self.properties.get(&(*object, *property, *start)).cloned()
                };
                Some(Response::Property {
                    object: *object,
                    property: *property,
                    start: *start,
                    count: if data.is_some() { *count } else { 0 },
                    data: data.unwrap_or_default(),
                })
            }
            Request::PropertyWrite {
                object,
                property,
                start,
                count,
                data,
            } => {
                let answer = match data.as_slice() {
                    [event, ..] if *property == 5 && data.len() == 10 => {
                        vec![self.load_event(*object, *event)]
                    }
                    _ => {
                        self.properties
                            .insert((*object, *property, *start), data.clone());
                        data.clone()
                    }
                };
                Some(Response::Property {
                    object: *object,
                    property: *property,
                    start: *start,
                    count: *count,
                    data: answer,
                })
            }
            Request::MemoryRead { address, count } => {
                let states_only = (0..u16::from(*count)).all(|offset| {
                    self.state_machine_at(address.wrapping_add(offset))
                        .is_some()
                });
                if self.memory_refused && !states_only {
                    return Some(Response::Memory {
                        address: *address,
                        data: Vec::new(),
                    });
                }
                let data = (0..u16::from(*count))
                    .map(|offset| {
                        let at = address.wrapping_add(offset);
                        match self.state_machine_at(at) {
                            Some(object) => self.load_states.get(&object).copied().unwrap_or(0),
                            None => self.memory(at),
                        }
                    })
                    .collect();
                Some(Response::Memory {
                    address: *address,
                    data,
                })
            }
            Request::MemoryWrite { address, data } => {
                if let (Some((control, _)), [head, ..]) = (memory_control, data.as_slice())
                    && control == *address
                {
                    self.load_event(head >> 4, head & 0x0F);
                    return None;
                }
                self.set_memory(*address, data);
                self.echoes_writes().then(|| Response::Memory {
                    address: *address,
                    data: data.clone(),
                })
            }
        }
    }

    /// Keep the model in step with a recorded answer.
    fn absorb(&mut self, response: &Response) {
        match response {
            Response::Memory { address, data } => self.set_memory(*address, data),
            Response::Property {
                object,
                property,
                start,
                count,
                data,
            } if *count > 0 && *property != 5 => {
                self.properties
                    .insert((*object, *property, *start), data.clone());
            }
            Response::Property {
                object,
                property: 5,
                data,
                ..
            } => {
                if let Some(state) = data.first() {
                    self.load_states.insert(*object, *state);
                }
            }
            _ => {}
        }
    }
}

impl Device for SimulatedDevice {
    fn exchange(&mut self, request: &Request, _expect: Expect) -> Result<Option<Response>, Fault> {
        Ok(self.answer(request))
    }
}

impl SimulatedDevice {
    /// Handle one request: the recorded answer if the recording has one left for it, else the
    /// model's. `None` when the device gives no answer.
    pub fn answer(&mut self, request: &Request) -> Option<Response> {
        let modelled = self.model(request);
        let recorded = self
            .script
            .get_mut(request)
            .and_then(|queue| queue.pop_front());
        match recorded {
            Some(answer) => {
                self.scripted += 1;
                if let Some(response) = &answer {
                    self.absorb(response);
                }
                answer
            }
            None => {
                self.modelled += 1;
                modelled
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requests(lines: &[&str]) -> Vec<Request> {
        lines.iter().map(|line| line.parse().unwrap()).collect()
    }

    fn responses(lines: &[&str]) -> Vec<Response> {
        lines.iter().map(|line| line.parse().unwrap()).collect()
    }

    #[test]
    fn model_of_a_property_device() {
        let mut device = SimulatedDevice::new(0x0705);
        let ask = |device: &mut SimulatedDevice, line: &str| {
            device
                .answer(&line.parse().unwrap())
                .map(|response| response.to_string())
        };
        assert_eq!(ask(&mut device, "T_CONNECT"), None);
        assert_eq!(
            ask(&mut device, "DEVICE_DESCRIPTOR_READ").unwrap(),
            "DEVICE_DESCRIPTOR_RESPONSE 0705"
        );
        assert_eq!(
            ask(&mut device, "PROP_READ obj=0 pid=56 start=1 count=1").unwrap(),
            "PROP_RESPONSE obj=0 pid=56 start=1 count=0"
        );
        assert_eq!(
            ask(
                &mut device,
                "PROP_WRITE obj=1 pid=5 start=1 count=1 01000000000000000000"
            )
            .unwrap(),
            "PROP_RESPONSE obj=1 pid=5 start=1 count=1 02"
        );
        assert_eq!(
            ask(&mut device, "PROP_READ obj=1 pid=5 start=1 count=1").unwrap(),
            "PROP_RESPONSE obj=1 pid=5 start=1 count=1 02"
        );
        // No echo until verify mode is switched on.
        assert_eq!(ask(&mut device, "MEM_WRITE 4000 0102"), None);
        assert_eq!(
            ask(&mut device, "PROP_WRITE obj=0 pid=14 start=1 count=1 04").unwrap(),
            "PROP_RESPONSE obj=0 pid=14 start=1 count=1 04"
        );
        assert_eq!(
            ask(&mut device, "MEM_WRITE 4002 03").unwrap(),
            "MEM_RESPONSE 4002 03"
        );
        assert_eq!(
            ask(&mut device, "MEM_READ 4000 4").unwrap(),
            "MEM_RESPONSE 4000 01020300"
        );
        assert_eq!(device.modelled(), 9);
        assert_eq!(device.scripted(), 0);
    }

    #[test]
    fn model_of_a_memory_mapped_device() {
        let mut device = SimulatedDevice::new(0x0701);
        let load: Request = "MEM_WRITE 0104 2100000000000000000000".parse().unwrap();
        assert_eq!(device.answer(&load), None);
        let status: Request = "MEM_READ B6EB 1".parse().unwrap();
        assert_eq!(
            device.answer(&status).unwrap().to_string(),
            "MEM_RESPONSE B6EB 02"
        );
        let other: Request = "MEM_READ B6EA 1".parse().unwrap();
        assert_eq!(
            device.answer(&other).unwrap().to_string(),
            "MEM_RESPONSE B6EA 00"
        );
    }

    #[test]
    fn recording_is_replayed_per_request() {
        let recorded = requests(&[
            "T_CONNECT",
            "DEVICE_DESCRIPTOR_READ",
            "MEM_WRITE 0104 1100000000000000000000",
            "MEM_READ B6EA 1",
            "MEM_READ 4000 2",
            "MEM_READ B6EA 1",
            "RESTART",
        ]);
        let answers = responses(&[
            "DEVICE_DESCRIPTOR_RESPONSE 0701",
            "MEM_RESPONSE B6EA 02",
            "MEM_RESPONSE 4000 AABB",
            "MEM_RESPONSE B6EA 07",
        ]);
        let mut device = SimulatedDevice::from_recording(0x0701, &recorded, &answers).unwrap();
        let status: Request = "MEM_READ B6EA 1".parse().unwrap();
        assert_eq!(
            device.answer(&status).unwrap().to_string(),
            "MEM_RESPONSE B6EA 02"
        );
        assert_eq!(
            device.answer(&status).unwrap().to_string(),
            "MEM_RESPONSE B6EA 07"
        );
        // Asked more often than recorded: the model takes over.
        assert_eq!(
            device.answer(&status).unwrap().to_string(),
            "MEM_RESPONSE B6EA 00"
        );
        // The recorded memory content is known to the model, too.
        let read: Request = "MEM_READ 4001 1".parse().unwrap();
        let memory: Request = "MEM_READ 4000 2".parse().unwrap();
        device.answer(&memory);
        assert_eq!(
            device.answer(&read).unwrap().to_string(),
            "MEM_RESPONSE 4001 BB"
        );
        assert_eq!((device.scripted(), device.modelled()), (3, 2));
    }

    #[test]
    fn recordings_that_do_not_pair_are_rejected() {
        let recorded = requests(&["DEVICE_DESCRIPTOR_READ", "MEM_READ 4000 1"]);
        let missing = responses(&["DEVICE_DESCRIPTOR_RESPONSE 0701"]);
        assert_eq!(
            SimulatedDevice::from_recording(0x0701, &recorded, &missing).unwrap_err(),
            RecordingError { response: 1 }
        );
        let surplus = responses(&[
            "DEVICE_DESCRIPTOR_RESPONSE 0701",
            "MEM_RESPONSE 4000 00",
            "MEM_RESPONSE 4000 00",
        ]);
        assert!(SimulatedDevice::from_recording(0x0701, &recorded, &surplus).is_err());
    }
}
