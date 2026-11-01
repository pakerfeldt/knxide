//! Property tests for addresses and cEMI frames.

use knxide_core::cemi::{Cemi, LData, Priority};
use knxide_core::{
    Apdu, Destination, GroupAddress, GroupAddressStyle, IndividualAddress, Payload, Tpdu,
};
use proptest::prelude::*;

fn payload() -> impl Strategy<Value = Payload> {
    prop_oneof![
        (0u8..=0x3F).prop_map(Payload::Small),
        prop::collection::vec(any::<u8>(), 1..20).prop_map(Payload::Bytes),
    ]
}

/// APDUs in canonical form: `Other` never uses a group value APCI.
fn apdu() -> impl Strategy<Value = Apdu> {
    prop_oneof![
        Just(Apdu::GroupValueRead),
        payload().prop_map(Apdu::GroupValueResponse),
        payload().prop_map(Apdu::GroupValueWrite),
        (0x0C0u16..=0x3FF, prop::collection::vec(any::<u8>(), 0..20))
            .prop_map(|(apci, data)| Apdu::Other { apci, data }),
    ]
}

fn tpdu() -> impl Strategy<Value = Tpdu> {
    prop_oneof![
        apdu().prop_map(Tpdu::Data),
        apdu().prop_map(Tpdu::TagGroup),
        (0u8..16, apdu()).prop_map(|(sequence, apdu)| Tpdu::Connected { sequence, apdu }),
        Just(Tpdu::Connect),
        Just(Tpdu::Disconnect),
        (0u8..16).prop_map(|sequence| Tpdu::Ack { sequence }),
        (0u8..16).prop_map(|sequence| Tpdu::Nak { sequence }),
    ]
}

fn priority() -> impl Strategy<Value = Priority> {
    prop_oneof![
        Just(Priority::System),
        Just(Priority::Normal),
        Just(Priority::Urgent),
        Just(Priority::Low)
    ]
}

prop_compose! {
    fn ldata()(
        additional_info in prop::collection::vec(any::<u8>(), 0..8),
        source in any::<u16>(),
        group in any::<bool>(),
        dst in any::<u16>(),
        priority in priority(),
        flags in any::<[bool; 4]>(),
        hop_count in 0u8..8,
        extended_frame_format in 0u8..16,
        tpdu in tpdu(),
    ) -> LData {
        let destination = if group {
            Destination::Group(GroupAddress::from_raw(dst))
        } else {
            Destination::Individual(IndividualAddress::from_raw(dst))
        };
        LData {
            additional_info,
            source: IndividualAddress::from_raw(source),
            destination,
            priority,
            repeat_on_error: flags[0],
            system_broadcast: flags[1],
            ack_request: flags[2],
            confirm_error: flags[3],
            hop_count,
            extended_frame_format,
            tpdu,
        }
    }
}

fn cemi() -> impl Strategy<Value = Cemi> {
    prop_oneof![
        ldata().prop_map(Cemi::LDataReq),
        ldata().prop_map(Cemi::LDataCon),
        ldata().prop_map(Cemi::LDataInd),
    ]
}

proptest! {
    #[test]
    fn individual_address_text_round_trip(raw in any::<u16>()) {
        let ia = IndividualAddress::from_raw(raw);
        prop_assert_eq!(ia.to_string().parse::<IndividualAddress>(), Ok(ia));
    }

    #[test]
    fn group_address_text_round_trip(raw in any::<u16>()) {
        let ga = GroupAddress::from_raw(raw);
        for style in [GroupAddressStyle::ThreeLevel, GroupAddressStyle::TwoLevel, GroupAddressStyle::Free] {
            prop_assert_eq!(ga.display(style).to_string().parse::<GroupAddress>(), Ok(ga));
        }
    }

    #[test]
    fn cemi_round_trip(frame in cemi()) {
        let bytes = frame.to_vec().unwrap();
        prop_assert_eq!(Cemi::decode(&bytes), Ok(frame));
    }

    /// Any input either fails to decode or decodes to a frame that re-encodes to something that
    /// decodes identically. Only the frame type bit may differ: it is derived, not stored.
    #[test]
    fn cemi_decode_arbitrary(bytes in prop::collection::vec(any::<u8>(), 0..40)) {
        if let Ok(frame) = Cemi::decode(&bytes) {
            let again = frame.to_vec().unwrap();
            prop_assert_eq!(again.len(), bytes.len());
            prop_assert_eq!(Cemi::decode(&again), Ok(frame));
        }
    }
}
