//! The messages whose codec `#[derive(Encode, Decode)]` writes keep the
//! bytes their hand-written codec wrote: each is checked against that codec,
//! kept here, and read back.

use viso_dev_protocol::inspect::{InspectAsk, MAX_ASKED_NODES, NodeSource};
use viso_ende::{Decode, DecodeError, Encode, Encoder};

/// `InspectAsk` as the dev link wrote it by hand.
fn ask_by_hand(ask: &InspectAsk) -> Vec<u8> {
    let mut enc = Encoder::new();
    match ask {
        InspectAsk::Census => enc.write_u8(0),
        InspectAsk::Nodes { window, nodes } => {
            enc.write_u8(1);
            enc.write_varint(u64::from(*window));
            enc.write_varint(nodes.len() as u64);
            for (index, generation) in nodes {
                enc.write_varint(u64::from(*index));
                enc.write_varint(u64::from(*generation));
            }
        }
        InspectAsk::Frame { window } => {
            enc.write_u8(2);
            enc.write_varint(u64::from(*window));
        }
        InspectAsk::Events { window, after } => {
            enc.write_u8(3);
            enc.write_varint(u64::from(*window));
            enc.write_varint(*after);
        }
    }
    enc.into_bytes()
}

/// `NodeSource` as the inspector wrote it by hand.
fn source_by_hand(node: &NodeSource) -> Vec<u8> {
    let mut enc = Encoder::new();
    let strings = |enc: &mut Encoder, values: &[String]| {
        enc.write_varint(values.len() as u64);
        for value in values {
            enc.write_str(value);
        }
    };
    enc.write_str(&node.type_name);
    match &node.name {
        Some(name) => {
            enc.write_u8(1);
            enc.write_str(name);
        }
        None => enc.write_u8(0),
    }
    strings(&mut enc, &node.styles);
    strings(&mut enc, &node.components);
    enc.write_str(&node.owner);
    let (bytes, start, end) = (node.bytes, node.start, node.end);
    for v in [bytes.0, bytes.1, start.0, start.1, end.0, end.1] {
        enc.write_varint(u64::from(v));
    }
    enc.write_varint(node.bindings.len() as u64);
    for (property, (hi, lo)) in &node.bindings {
        enc.write_str(property);
        enc.write_u64(*hi);
        enc.write_u64(*lo);
    }
    enc.into_bytes()
}

#[test]
fn an_inspect_ask_keeps_its_bytes() {
    let asks = [
        InspectAsk::Census,
        InspectAsk::Nodes {
            window: 300,
            nodes: vec![(0, 1), (70_000, u32::MAX)],
        },
        InspectAsk::Frame { window: 2 },
        InspectAsk::Events {
            window: 1,
            after: u64::MAX,
        },
    ];
    for ask in asks {
        let bytes = ask.encode_to_vec();
        assert_eq!(bytes, ask_by_hand(&ask), "{ask:?}");
        assert_eq!(InspectAsk::decode_from_slice(&bytes), Ok(ask));
    }
}

#[test]
fn an_inspect_ask_past_its_cap_or_tags_does_not_read() {
    let mut enc = Encoder::new();
    enc.write_u8(1);
    enc.write_varint(0);
    enc.write_varint(MAX_ASKED_NODES as u64 + 1);
    assert!(matches!(
        InspectAsk::decode_from_slice(&enc.into_bytes()),
        Err(DecodeError::TooMany { offset: 2, .. } | DecodeError::UnexpectedEof { .. })
    ));
    assert_eq!(
        InspectAsk::decode_from_slice(&[4]),
        Err(DecodeError::Malformed { offset: 0 })
    );
    let mut enc = Encoder::new();
    enc.write_u8(2);
    enc.write_varint(u64::from(u32::MAX) + 1);
    assert_eq!(
        InspectAsk::decode_from_slice(&enc.into_bytes()),
        Err(DecodeError::Malformed { offset: 1 }),
        "a window past `u32`"
    );
}

#[test]
fn a_node_source_keeps_its_bytes() {
    let nodes = [
        NodeSource::default(),
        NodeSource {
            type_name: "Button".into(),
            name: Some("save".into()),
            styles: vec!["primary".into(), "wide".into()],
            components: vec!["App".into()],
            owner: "App".into(),
            bytes: (12, 400),
            start: (3, 5),
            end: (9, 1),
            bindings: vec![("text".into(), (u64::MAX, 7))],
        },
    ];
    for node in nodes {
        let bytes = node.encode_to_vec();
        assert_eq!(bytes, source_by_hand(&node), "{node:?}");
        assert_eq!(NodeSource::decode_from_slice(&bytes), Ok(node));
    }
}
