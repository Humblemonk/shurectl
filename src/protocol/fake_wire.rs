//! Request parsing and reply framing for the fake device in `fake_hid.rs`, so
//! the byte layout stays in the protocol module, not the fake. Built only for
//! tests and `--features qa`.

use super::{
    CMD_CONFIRM, CMD_GET_FEAT, CMD_GET_LOCK, CMD_SET_FEAT, CMD_SET_LOCK, DATA_START, DeviceModel,
    HDR_CONSTANT, HDR_END, HEADER_MAGIC, PACKET_SIZE, PREFIX_OFFSET, REPORT_ID, RES_CONFIRM_ACK,
    RES_GET_FEAT, RES_GET_LOCK, RES_SET_FEAT, RES_SET_LOCK, Reply, build_packet, crc16_ansi,
    encode_lp_string,
};

/// The MVX2U Gen 2's ack to a SET, sent before the CONFIRM ack (checked on
/// hardware). `parse_response()` doesn't decode it. What follows the command
/// bytes is not captured; the fake sends the SET's prefix and address.
const RES_SET_ACK: [u8; 3] = [0x0a, 0x02, 0x02];

/// The header byte `build_packet_hdr0()` writes in place of `HDR_CONSTANT`.
const HDR_ZERO: u8 = 0x00;

/// The two command classes. A GET and a SET of one setting share a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    Feature,
    Lock,
}

/// Where a request points: its class, then the prefix byte and the 2-byte
/// feature address exactly as the request carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub class: Class,
    pub addr: [u8; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Get(Key),
    Set(Key, Vec<u8>),
    Confirm,
}

/// The header byte a packet was built with: `build_packet()` or
/// `build_packet_hdr0()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    Standard,
    Hdr0,
}

/// One parsed host packet, with the framing and sequence byte it carried.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub request: Request,
    pub framing: Framing,
    pub seq: u8,
}

/// The framing a SET must use on `model`, given the framing of the
/// constructor that builds it for the other binary models. Every MV7+ SET
/// uses `build_packet_hdr0()` (MOTIV capture, firmware 1.6); elsewhere the
/// constructor decides (Mic and Playback Level, the mute button and factory
/// reset use it too).
pub fn set_framing(model: DeviceModel, constructor: Framing) -> Framing {
    match model {
        DeviceModel::Mv7Plus => Framing::Hdr0,
        DeviceModel::Mvx2u
        | DeviceModel::Mvx2uGen2
        | DeviceModel::Mv6
        | DeviceModel::Mv6Gen2
        | DeviceModel::Mv7 => constructor,
    }
}

/// The framing every GET and CONFIRM uses.
pub const QUERY_FRAMING: Framing = Framing::Standard;

/// Parse one host packet. `None` unless it is a well-formed 64-byte packet
/// with a valid CRC, one of the two header bytes and a known command.
/// Whether that header byte is the right one for the command, and the
/// sequence byte the next one, is for the caller to check.
pub fn parse_request(buf: &[u8]) -> Option<Parsed> {
    if buf.len() != PACKET_SIZE || buf[0] != REPORT_ID || buf[2..4] != HEADER_MAGIC {
        return None;
    }
    let framing = match buf[5] {
        HDR_CONSTANT => Framing::Standard,
        HDR_ZERO => Framing::Hdr0,
        _ => return None,
    };
    let seq = buf[4];
    let request = parse_command(buf)?;
    Some(Parsed {
        request,
        framing,
        seq,
    })
}

fn parse_command(buf: &[u8]) -> Option<Request> {
    let contents_end = usize::from(buf[1]);
    if contents_end < PREFIX_OFFSET || contents_end + 2 > buf.len() {
        return None;
    }
    let crc = u16::from_be_bytes([buf[contents_end], buf[contents_end + 1]]);
    let data_len = usize::from(buf[7]);
    if crc != crc16_ansi(&buf[2..contents_end])
        || buf[6] != HDR_END
        || buf[8] != DATA_START
        || buf[9] != buf[7]
        || data_len + 8 != contents_end
    {
        return None;
    }
    let cmd: [u8; 3] = buf[10..PREFIX_OFFSET].try_into().ok()?;
    if cmd == CMD_CONFIRM {
        return (contents_end == PREFIX_OFFSET).then_some(Request::Confirm);
    }
    let addr: [u8; 3] = buf.get(PREFIX_OFFSET..PREFIX_OFFSET + 3)?.try_into().ok()?;
    if contents_end < PREFIX_OFFSET + 3 {
        return None;
    }
    let value = buf[PREFIX_OFFSET + 3..contents_end].to_vec();
    let key = |class| Key { class, addr };
    match cmd {
        c if c == CMD_GET_FEAT && value.is_empty() => Some(Request::Get(key(Class::Feature))),
        c if c == CMD_GET_LOCK && value.is_empty() => Some(Request::Get(key(Class::Lock))),
        c if c == CMD_SET_FEAT => Some(Request::Set(key(Class::Feature), value)),
        c if c == CMD_SET_LOCK => Some(Request::Set(key(Class::Lock), value)),
        _ => None,
    }
}

/// A GET reply carrying `value`, echoing the request's sequence byte,
/// prefix and address.
pub fn reply(seq: u8, key: Key, value: &[u8]) -> Vec<u8> {
    let cmd = match key.class {
        Class::Feature => RES_GET_FEAT,
        Class::Lock => RES_GET_LOCK,
    };
    let mut payload = key.addr.to_vec();
    payload.extend_from_slice(value);
    build_packet(seq, &cmd, &payload)
}

/// The unsolicited reports a binary model sends after a CONFIRM, in order,
/// for the SET it confirmed (`None` for a CONFIRM with no SET before it).
/// `DeviceModel::reports_after_confirm()` says how many `send_set()` reads.
///
/// - MV7+ (MOTIV capture, firmware 1.6): the CONFIRM ack (`09 00 00`) and an
///   echo of the SET (`04 02 02` and its address and value; `04 02 01` for
///   the lock class), in either order (echo first 71 times, ack first 47).
///   LED-page SETs sometimes send the ack alone (20 of 52). Both came within
///   19 ms of the CONFIRM. The fake sends the ack first. The echo parses as
///   a reply, so one left unread is decoded by a later GET.
/// - MVX2U Gen 2 (checked on hardware): a SET ack (`0a 02 02`), then the
///   CONFIRM ack.
/// - MVX2U Gen 1, MV6 and MV6 Gen 2: unchecked; the fake assumes the MV7+
///   order, the one a readback can misread.
/// - MV7: no binary acks.
pub fn acks_after_confirm(model: DeviceModel, set: Option<(Key, &[u8])>) -> Vec<Vec<u8>> {
    let confirm = build_packet(0, &RES_CONFIRM_ACK, &[]);
    let Some((key, value)) = set else {
        return vec![confirm];
    };
    let mut payload = key.addr.to_vec();
    match model {
        DeviceModel::Mvx2uGen2 => {
            vec![build_packet(0, &RES_SET_ACK, &payload), confirm]
        }
        DeviceModel::Mvx2u | DeviceModel::Mv6 | DeviceModel::Mv6Gen2 | DeviceModel::Mv7Plus => {
            let echo = match key.class {
                Class::Feature => RES_SET_FEAT,
                Class::Lock => RES_SET_LOCK,
            };
            payload.extend_from_slice(value);
            vec![confirm, build_packet(0, &echo, &payload)]
        }
        DeviceModel::Mv7 => Vec::new(),
    }
}

/// `value` as the device would hold it, decoded as if it were a GET reply.
pub fn as_reply(key: Key, value: &[u8]) -> Reply {
    Reply {
        prefix: key.addr[0],
        feat: [key.addr[1], key.addr[2]],
        value: value.to_vec(),
    }
}

/// An identity string (firmware version, serial) as its GET reply carries it.
pub fn lp_string(text: &str) -> Vec<u8> {
    encode_lp_string(text.as_bytes())
}
