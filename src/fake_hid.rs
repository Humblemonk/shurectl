//! A fake Shure device for tests and the `--qa` harness. Built only for tests
//! and `--features qa`, so it never ships.
//!
//! The fake agrees with the app by construction: every address and encoding it
//! knows comes from the `protocol.rs` constructors, so a wrong feature address or
//! value encoding passes both sides. It catches sequencing, routing, UI and state
//! bugs, never a protocol misreading. Never cite it as evidence that a packet is
//! right; hardware truth comes from usbmon captures and the roundtrip tests in
//! `protocol.rs`.
//!
//! Binary models: a SET is stored when its CONFIRM arrives, a GET is answered from
//! the store, and each CONFIRM queues the acks `fake_wire::acks_after_confirm()`
//! lists for the model, one of which parses as a reply on the MV7+. A packet with
//! the wrong header byte for its command, or a sequence byte that doesn't follow
//! the previous one on its handle, is refused. The MV7 runs
//! `mv7_text::shell` instead. Faults (dropped or extra acks, failed I/O, refused
//! commands, unplugging) give disconnect and readback-order tests something to
//! bite on. Every packet received is logged, decoded by name.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::device::{Disconnected, HidTransport, ShureDevice};
use crate::protocol::fake_wire::{self, Framing, Key, Parsed, Request};
use crate::protocol::{
    self, CompressorPreset, DeviceModel, DeviceState, HpfFrequency, InputMode, PACKET_SIZE,
    apply_reply, mv7_text,
};

/// An address no setting uses, for replies to a refused GET.
const UNKNOWN_ADDR: [u8; 3] = [0xFF, 0xFF, 0xFF];

/// One packet the fake received.
#[derive(Debug, Clone, PartialEq)]
pub struct Packet {
    /// The bytes that carry data: a binary packet up to its CRC, or an MV7 line.
    pub raw: Vec<u8>,
    pub kind: PacketKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PacketKind {
    Get {
        feature: String,
    },
    Set {
        feature: String,
        /// The value decoded the way a GET reply of it would be, or its hex bytes.
        value: Value,
        /// False when the device ignored it (a gain write while Gain Lock is on).
        applied: bool,
    },
    Confirm,
    Text {
        line: String,
        reply: String,
    },
    /// Not a packet `protocol.rs` would build; the device ignores it.
    Refused {
        reason: String,
    },
}

impl Packet {
    pub fn to_json(&self) -> Value {
        let raw = hex(&self.raw);
        match &self.kind {
            PacketKind::Get { feature } => json!({ "get": feature, "raw": raw }),
            PacketKind::Set {
                feature,
                value,
                applied,
            } => json!({ "set": feature, "value": value, "applied": applied, "raw": raw }),
            PacketKind::Confirm => json!({ "confirm": true, "raw": raw }),
            PacketKind::Text { line, reply } => json!({ "text": line, "reply": reply }),
            PacketKind::Refused { reason } => json!({ "refused": reason, "raw": raw }),
        }
    }
}

/// Bytes as space-separated hex, the way usbmon captures are quoted.
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Misbehaviour to inject. Ack and refusal counts are used up as they apply.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Faults {
    /// Acks to leave out after the next CONFIRMs.
    pub drop_acks: usize,
    /// Extra acks to send after the next CONFIRM.
    pub extra_acks: usize,
    /// Every read and write fails, as on a stale handle.
    pub fail_io: bool,
    /// Commands to answer with an unknown address (binary) or `[Failed]` (MV7).
    pub refuse: usize,
}

type ValueOf = Box<dyn Fn(&DeviceState) -> Vec<u8>>;
type ReadOf = Box<dyn Fn(&DeviceState) -> Value>;

/// One setting the fake stores, named for the packet log.
struct Feature {
    name: String,
    key: Key,
    /// The framing of the constructor the key came from; see `fake_wire::set_framing()`.
    framing: Framing,
    /// The value bytes for a state, as a SET carries them and a GET reply returns them.
    value: ValueOf,
    /// The decoded value for the packet log. `None` where a reply can't be decoded
    /// on its own (Mic Preset select, the LED pulsing theme that aliases the lock).
    read: Option<ReadOf>,
}

/// A feature whose SET constructor carries the value: key and value come from
/// parsing the packet `set` builds.
fn set_feature(
    name: &str,
    set: impl Fn(&DeviceState) -> Vec<u8> + 'static,
    read: Option<fn(&DeviceState) -> Value>,
) -> Option<Feature> {
    let Some(Parsed {
        request: Request::Set(key, _),
        framing,
        ..
    }) = fake_wire::parse_request(&set(&DeviceState::default()))
    else {
        return None;
    };
    let value = move |state: &DeviceState| match fake_wire::parse_request(&set(state)) {
        Some(Parsed {
            request: Request::Set(_, value),
            ..
        }) => value,
        Some(_) | None => Vec::new(),
    };
    Some(Feature {
        name: name.into(),
        key,
        framing,
        value: Box::new(value),
        read: read.map(|read| Box::new(read) as ReadOf),
    })
}

/// `feature` with its log value read by a closure, for the entries that are
/// built in a loop (EQ bands, LED colors).
fn with_read(
    feature: Option<Feature>,
    read: impl Fn(&DeviceState) -> Value + 'static,
) -> Option<Feature> {
    feature.map(|feature| Feature {
        read: Some(Box::new(read)),
        ..feature
    })
}

/// A read-only identity string: the key comes from its GET constructor.
fn string_feature(
    name: &str,
    get: fn(u8) -> Vec<u8>,
    read: fn(&DeviceState) -> Value,
) -> Option<Feature> {
    let Some(Parsed {
        request: Request::Get(key),
        framing,
        ..
    }) = fake_wire::parse_request(&get(0))
    else {
        return None;
    };
    let value = move |state: &DeviceState| match read(state) {
        Value::String(text) => fake_wire::lp_string(&text),
        _ => Vec::new(),
    };
    Some(Feature {
        name: name.into(),
        key,
        framing,
        value: Box::new(value),
        read: Some(Box::new(read)),
    })
}

fn debug_json(value: impl std::fmt::Debug) -> Value {
    Value::String(format!("{value:?}"))
}

/// Every setting the fake knows, from one shared list. Keys a model never sends
/// are harmless: the fake answers whatever `device.rs` asks for.
fn features(model: DeviceModel) -> Vec<Feature> {
    use protocol::*;
    let mut list = vec![
        set_feature(
            "GAIN",
            |s| cmd_set_gain(0, s.gain_tenths),
            Some(|s| json!(s.gain_tenths)),
        ),
        set_feature(
            "MUTE",
            |s| cmd_set_mute(0, s.muted),
            Some(|s| json!(s.muted)),
        ),
        set_feature(
            "HPF",
            |s| cmd_set_hpf(0, &s.hpf),
            Some(|s| debug_json(s.hpf)),
        ),
        set_feature(
            "LIMITER",
            |s| cmd_set_limiter(0, s.limiter_enabled),
            Some(|s| json!(s.limiter_enabled)),
        ),
        set_feature(
            "COMPRESSOR",
            |s| cmd_set_compressor(0, &s.compressor),
            Some(|s| debug_json(s.compressor)),
        ),
        set_feature(
            "PHANTOM",
            |s| cmd_set_phantom(0, s.phantom_power),
            Some(|s| json!(s.phantom_power)),
        ),
        set_feature(
            "MODE",
            |s| cmd_set_mode(0, s.mode == InputMode::Auto),
            Some(|s| debug_json(s.mode)),
        ),
        set_feature(
            "AUTO_POSITION",
            |s| cmd_set_auto_position(0, &s.auto_position),
            Some(|s| debug_json(s.auto_position)),
        ),
        set_feature(
            "AUTO_TONE",
            |s| cmd_set_auto_tone(0, &s.auto_tone),
            Some(|s| debug_json(s.auto_tone)),
        ),
        set_feature(
            "AUTO_GAIN",
            |s| cmd_set_auto_gain(0, &s.auto_gain),
            Some(|s| debug_json(s.auto_gain)),
        ),
        // The Gen 1 balance (prefix 0x01) and the Mic Level (prefix 0x00) are
        // different keys at one address; Playback Level is prefix 0x03.
        set_feature(
            "MONITOR_MIX",
            |s| cmd_set_mix(0, s.monitor_mix),
            Some(|s| json!(s.monitor_mix)),
        ),
        set_feature(
            "MIC_LEVEL",
            |s| cmd_set_mv6_mix(0, s.monitor_mix),
            Some(|s| json!(s.monitor_mix)),
        ),
        set_feature(
            "PLAYBACK_LEVEL",
            |s| cmd_set_mv7_playback_mix(0, s.playback_mix),
            Some(|s| json!(s.playback_mix)),
        ),
        set_feature(
            "EQ",
            |s| cmd_set_eq_enable(0, s.eq_enabled),
            Some(|s| json!(s.eq_enabled)),
        ),
        set_feature(
            "LOCK",
            |s| cmd_set_lock(0, s.locked),
            Some(|s| json!(s.locked)),
        ),
        set_feature(
            "DEVICE_NAME",
            |s| cmd_set_device_name(0, &s.device_name),
            Some(|s| json!(s.device_name)),
        ),
        string_feature("FIRMWARE", cmd_get_firmware_version, |s| {
            json!(s.firmware_version)
        }),
        string_feature("SERIAL", cmd_get_serial, |s| json!(s.factory_serial)),
        set_feature(
            "DENOISER",
            |s| cmd_set_mv6_denoiser(0, s.denoiser_enabled),
            Some(|s| json!(s.denoiser_enabled)),
        ),
        set_feature(
            "POPPER_STOPPER",
            |s| cmd_set_mv6_popper_stopper(0, s.popper_stopper_enabled),
            Some(|s| json!(s.popper_stopper_enabled)),
        ),
        set_feature(
            "MUTE_BUTTON",
            |s| cmd_set_mv6_mute_btn_disable(0, s.mute_btn_disabled),
            Some(|s| json!(s.mute_btn_disabled)),
        ),
        set_feature(
            "TONE",
            |s| cmd_set_mv6_tone(0, s.tone),
            Some(|s| json!(s.tone)),
        ),
        set_feature(
            "GAIN_LOCK",
            |s| cmd_set_mv6_gain_lock(0, s.mv6_gain_locked),
            Some(|s| json!(s.mv6_gain_locked)),
        ),
        set_feature(
            "ADAPTATION_RATE",
            |s| cmd_set_adaptation_rate(0, s.adaptation_rate_fast),
            Some(|s| json!(s.adaptation_rate_fast)),
        ),
        set_feature(
            "DENOISER_LEVEL",
            |s| cmd_set_denoiser_level(0, s.denoiser_level),
            Some(|s| json!(s.denoiser_level)),
        ),
        set_feature(
            "TONE_MODE",
            |s| cmd_set_tone_mode(0, s.tone_advanced),
            Some(|s| json!(s.tone_advanced)),
        ),
        set_feature(
            "MIC_PRESET",
            |s| cmd_set_mic_preset(0, MicPreset::matching(s).unwrap_or(MicPreset::Speech)),
            None,
        ),
        set_feature(
            "REVERB_OUTPUT",
            |s| cmd_set_mv7_reverb_output(0, s.reverb_on_output),
            Some(|s| json!(s.reverb_on_output)),
        ),
        set_feature(
            "REVERB_TYPE",
            |s| cmd_set_mv7_reverb_type(0, &s.reverb_type),
            Some(|s| debug_json(s.reverb_type)),
        ),
        set_feature(
            "REVERB_INTENSITY",
            |s| cmd_set_mv7_reverb_intensity(0, s.reverb_intensity),
            Some(|s| json!(s.reverb_intensity)),
        ),
        set_feature(
            "REVERB_MONITOR",
            |s| cmd_set_mv7_reverb_monitor(0, s.reverb_monitoring),
            Some(|s| json!(s.reverb_monitoring)),
        ),
        set_feature(
            "LED_BEHAVIOR",
            |s| cmd_set_mv7_led_behavior(0, s.led_behavior),
            Some(|s| debug_json(s.led_behavior)),
        ),
        set_feature(
            "LED_BRIGHTNESS",
            |s| cmd_set_mv7_led_brightness(0, s.led_brightness),
            Some(|s| debug_json(s.led_brightness)),
        ),
        set_feature(
            "LED_LIVE_THEME",
            |s| cmd_set_mv7_led_live_theme(0, s.led_live_theme),
            Some(|s| debug_json(s.led_live_theme)),
        ),
        set_feature(
            "LED_SOLID_THEME",
            |s| cmd_set_mv7_led_solid_theme(0, s.led_solid_theme),
            Some(|s| debug_json(s.led_solid_theme)),
        ),
        // Its reply address aliases the config lock, so it is never read back.
        set_feature(
            "LED_PULSING_THEME",
            |s| cmd_set_mv7_led_pulsing_theme(0, s.led_pulsing_theme),
            None,
        ),
    ];
    let rgb_features: [(&str, RgbOf, RgbSet); 5] = [
        (
            "LED_SOLID_COLOR",
            |s| s.led_solid_rgb,
            cmd_set_mv7_led_solid_color,
        ),
        (
            "LED_PULSING_COLOR",
            |s| s.led_pulsing_rgb,
            cmd_set_mv7_led_pulsing_color,
        ),
        (
            "LED_LIVE_EDGE",
            |s| s.led_live_edge_rgb,
            cmd_set_mv7_led_live_edge,
        ),
        (
            "LED_LIVE_MIDDLE",
            |s| s.led_live_middle_rgb,
            cmd_set_mv7_led_live_middle,
        ),
        (
            "LED_LIVE_INTERIOR",
            |s| s.led_live_interior_rgb,
            cmd_set_mv7_led_live_interior,
        ),
    ];
    for (name, rgb, set) in rgb_features {
        let set = move |s: &DeviceState| {
            let [r, g, b] = rgb(s);
            set(0, r, g, b)
        };
        list.push(with_read(set_feature(name, set, None), move |s| {
            json!(rgb(s))
        }));
    }
    for band in 0..protocol::EQ_BAND_FREQS.len() {
        let enable = set_feature(
            &format!("EQ_BAND_{}_ENABLE", band + 1),
            move |s| cmd_set_eq_band_enable(0, band, s.eq_bands[band].enabled),
            None,
        );
        list.push(with_read(enable, move |s| json!(s.eq_bands[band].enabled)));
        let gain = set_feature(
            &format!("EQ_BAND_{}_GAIN", band + 1),
            move |s| cmd_set_eq_band_gain(0, band, s.eq_bands[band].gain_db, model),
            None,
        );
        list.push(with_read(gain, move |s| json!(s.eq_bands[band].gain_db)));
    }
    list.into_iter().flatten().collect()
}

type RgbSet = fn(u8, u8, u8, u8) -> Vec<u8>;
type RgbOf = fn(&DeviceState) -> [u8; 3];

/// The decoded value of `value` at `feature`, by applying it as a GET reply.
fn decode(feature: &Feature, value: &[u8]) -> Value {
    let mut state = DeviceState::default();
    match &feature.read {
        Some(read) if apply_reply(&fake_wire::as_reply(feature.key, value), &mut state) => {
            read(&state)
        }
        Some(_) | None => Value::String(hex(value)),
    }
}

/// The device side of the fake, shared by every handle opened on it.
struct FakeState {
    model: DeviceModel,
    seed: DeviceState,
    features: Vec<Feature>,
    store: HashMap<Key, Vec<u8>>,
    shell: Option<mv7_text::shell::Shell>,
    /// The last SET and its log index, applied when its CONFIRM arrives.
    pending_set: Option<(Key, Vec<u8>, usize)>,
    input: VecDeque<Vec<u8>>,
    log: Vec<Packet>,
    plugged: bool,
    /// Bumped on unplug and factory reset, so older handles fail like stale ones.
    generation: u32,
    /// Presence polls that still report the device absent while it reboots.
    rebooting_polls: u32,
    faults: Faults,
}

impl FakeState {
    fn new(model: DeviceModel, seed: DeviceState) -> Self {
        let mut state = Self {
            model,
            features: features(model),
            seed,
            store: HashMap::new(),
            shell: None,
            pending_set: None,
            input: VecDeque::new(),
            log: Vec::new(),
            plugged: true,
            generation: 0,
            rebooting_polls: 0,
            faults: Faults::default(),
        };
        state.load_seed();
        state
    }

    fn load_seed(&mut self) {
        self.store = self
            .features
            .iter()
            .map(|feature| (feature.key, (feature.value)(&self.seed)))
            .collect();
        self.shell =
            (self.model == DeviceModel::Mv7).then(|| mv7_text::shell::Shell::new(&self.seed));
        self.pending_set = None;
    }

    fn feature(&self, key: Key) -> Option<&Feature> {
        self.features.iter().find(|feature| feature.key == key)
    }

    fn feature_name(&self, key: Key) -> String {
        match self.feature(key) {
            Some(feature) => feature.name.clone(),
            None => format!("unrecognised {}", hex(&key.addr)),
        }
    }

    fn key_of(&self, name: &str) -> Option<Key> {
        self.features
            .iter()
            .find(|feature| feature.name == name)
            .map(|feature| feature.key)
    }

    fn handle_ok(&self, generation: u32) -> Result<()> {
        if !self.plugged || generation != self.generation || self.faults.fail_io {
            return Err(anyhow!("fake device I/O failed"));
        }
        Ok(())
    }

    fn receive(&mut self, packet: &[u8]) {
        if self.model == DeviceModel::Mv7 {
            self.receive_text(packet);
        } else {
            self.receive_binary(packet);
        }
    }

    fn receive_text(&mut self, packet: &[u8]) {
        let text: Vec<u8> = packet
            .iter()
            .skip(1)
            .copied()
            .take_while(|&b| b != b'\n')
            .collect();
        let well_formed = packet.len() == mv7_text::REPORT_SIZE
            && packet.first() == Some(&0x00)
            && packet.contains(&b'\n')
            && text.iter().all(u8::is_ascii_graphic_or_space);
        if !well_formed {
            self.refuse(packet, "not an MV7 command line");
            return;
        }
        let line = String::from_utf8_lossy(&text).into_owned();
        let reply = if self.faults.refuse > 0 {
            self.faults.refuse -= 1;
            mv7_text::FAILED_REPLY.to_string()
        } else {
            match &mut self.shell {
                Some(shell) => shell.answer(&line),
                None => mv7_text::FAILED_REPLY.to_string(),
            }
        };
        for chunk in format!("{reply}\n").as_bytes().chunks(PACKET_SIZE - 1) {
            let mut report = vec![0x00];
            report.extend_from_slice(chunk);
            report.resize(PACKET_SIZE, 0x00);
            self.input.push_back(report);
        }
        self.log.push(Packet {
            raw: text,
            kind: PacketKind::Text { line, reply },
        });
    }

    fn refuse(&mut self, packet: &[u8], reason: &str) {
        self.log.push(Packet {
            raw: packet.to_vec(),
            kind: PacketKind::Refused {
                reason: reason.into(),
            },
        });
    }

    /// The framing `request` must arrive with, or `None` where the fake can't
    /// tell (a SET to an address it doesn't know).
    fn expected_framing(&self, request: &Request) -> Option<Framing> {
        match request {
            Request::Get(_) | Request::Confirm => Some(fake_wire::QUERY_FRAMING),
            Request::Set(key, _) => self
                .feature(*key)
                .map(|feature| fake_wire::set_framing(self.model, feature.framing)),
        }
    }

    fn receive_binary(&mut self, packet: &[u8]) {
        let Some(Parsed {
            request,
            framing,
            seq,
        }) = fake_wire::parse_request(packet)
        else {
            self.refuse(packet, "not a well-formed packet");
            return;
        };
        let factory_reset = fake_wire::parse_request(&protocol::cmd_factory_reset(0));
        let is_factory_reset = factory_reset
            .as_ref()
            .is_some_and(|reset| reset.request == request);
        let expected = if is_factory_reset {
            factory_reset.map(|reset| reset.framing)
        } else {
            self.expected_framing(&request)
        };
        if let Some(expected) = expected.filter(|&expected| expected != framing) {
            self.refuse(
                packet,
                &format!("framed {framing:?}, but {request:?} uses {expected:?} on this model"),
            );
            return;
        }
        // Up to and including the CRC; the rest is zero padding.
        let raw = packet[..(usize::from(packet[1]) + 2).min(packet.len())].to_vec();
        match request {
            Request::Get(key) => {
                let feature = self.feature_name(key);
                self.log.push(Packet {
                    raw,
                    kind: PacketKind::Get { feature },
                });
                if self.faults.refuse > 0 {
                    self.faults.refuse -= 1;
                    let unknown = Key {
                        addr: UNKNOWN_ADDR,
                        ..key
                    };
                    self.input
                        .push_back(fake_wire::reply(seq, unknown, &[0x00]));
                } else if let Some(value) = self.store.get(&key) {
                    self.input.push_back(fake_wire::reply(seq, key, value));
                }
            }
            Request::Set(key, value) => {
                if is_factory_reset {
                    self.log.push(Packet {
                        raw,
                        kind: PacketKind::Set {
                            feature: "FACTORY_RESET".into(),
                            value: Value::String(hex(&value)),
                            applied: true,
                        },
                    });
                    self.factory_reset();
                    return;
                }
                let (feature, decoded) = match self.feature(key) {
                    Some(feature) => (feature.name.clone(), decode(feature, &value)),
                    None => (self.feature_name(key), Value::String(hex(&value))),
                };
                self.pending_set = Some((key, value, self.log.len()));
                self.log.push(Packet {
                    raw,
                    kind: PacketKind::Set {
                        feature,
                        value: decoded,
                        applied: false,
                    },
                });
            }
            Request::Confirm => {
                self.log.push(Packet {
                    raw,
                    kind: PacketKind::Confirm,
                });
                let confirmed = self.pending_set.take();
                if let Some((key, value, index)) = &confirmed {
                    let applied = !self.gain_locked_write(*key);
                    if applied {
                        self.store.insert(*key, value.clone());
                    }
                    if let Some(Packet {
                        kind: PacketKind::Set { applied: flag, .. },
                        ..
                    }) = self.log.get_mut(*index)
                    {
                        *flag = applied;
                    }
                }
                self.queue_acks(
                    confirmed
                        .as_ref()
                        .map(|(key, value, _)| (*key, value.as_slice())),
                );
            }
        }
    }

    /// A gain-locked device ignores gain writes (checked on hardware).
    fn gain_locked_write(&self, key: Key) -> bool {
        let (Some(gain), Some(lock)) = (self.key_of("GAIN"), self.key_of("GAIN_LOCK")) else {
            return false;
        };
        let mut state = DeviceState::default();
        key == gain
            && self.model.has_gain_lock()
            && self.store.get(&lock).is_some_and(|value| {
                apply_reply(&fake_wire::as_reply(lock, value), &mut state) && state.mv6_gain_locked
            })
    }

    /// Queue the model's acks for a CONFIRM of `set`. A dropped ack leaves out
    /// the last ones; an extra ack repeats the last.
    fn queue_acks(&mut self, set: Option<(Key, &[u8])>) {
        let mut acks = fake_wire::acks_after_confirm(self.model, set);
        if let Some(last) = acks.last().cloned() {
            acks.extend(std::iter::repeat_n(last, self.faults.extra_acks));
        }
        self.faults.extra_acks = 0;
        let dropped = self.faults.drop_acks.min(acks.len());
        self.faults.drop_acks -= dropped;
        acks.truncate(acks.len() - dropped);
        self.input.extend(acks);
    }

    /// The device erases its settings and reboots: handles go stale and the
    /// next presence poll misses it, as a real reset re-enumerates.
    fn factory_reset(&mut self) {
        self.seed = FakeDevice::seed_state(self.model);
        self.load_seed();
        self.input.clear();
        self.generation += 1;
        self.rebooting_polls = 1;
    }
}

trait AsciiLine {
    fn is_ascii_graphic_or_space(&self) -> bool;
}

impl AsciiLine for u8 {
    fn is_ascii_graphic_or_space(&self) -> bool {
        self.is_ascii_graphic() || *self == b' '
    }
}

/// The handle a test or the harness holds to control the fake device.
#[derive(Clone)]
pub struct FakeDevice(Rc<RefCell<FakeState>>);

impl FakeDevice {
    /// A fake `model` holding `seed_state(model)`.
    pub fn new(model: DeviceModel) -> Self {
        Self::with_state(model, Self::seed_state(model))
    }

    pub fn with_state(model: DeviceModel, state: DeviceState) -> Self {
        Self(Rc::new(RefCell::new(FakeState::new(model, state))))
    }

    /// The settings a fresh fake holds: the defaults, in Manual mode at 24 dB so
    /// gain is adjustable straight away, with identity strings that say "fake".
    pub fn seed_state(model: DeviceModel) -> DeviceState {
        DeviceState {
            mode: InputMode::Manual,
            gain_tenths: 240,
            hpf: HpfFrequency::Off,
            compressor: CompressorPreset::Off,
            device_name: format!("Fake {}", model.display_name()),
            firmware_version: "0.0.0-fake".into(),
            factory_serial: "FAKE0001".into(),
            ..DeviceState::default()
        }
    }

    pub fn model(&self) -> DeviceModel {
        self.0.borrow().model
    }

    /// A new `ShureDevice` handle on this fake, as `open()` would return.
    pub fn open(&self) -> ShureDevice {
        let generation = self.0.borrow().generation;
        ShureDevice::with_transport(self.model(), Box::new(FakeHid::new(&self.0, generation)))
    }

    pub fn unplug(&self) {
        let mut state = self.0.borrow_mut();
        state.plugged = false;
        state.generation += 1;
        state.input.clear();
    }

    /// Plug back in, keeping the settings, as a real device does.
    pub fn replug(&self) {
        self.0.borrow_mut().plugged = true;
    }

    pub fn faults_mut(&self, change: impl FnOnce(&mut Faults)) {
        change(&mut self.0.borrow_mut().faults);
    }

    /// Every packet received since `start`.
    pub fn log_since(&self, start: usize) -> Vec<Packet> {
        self.0
            .borrow()
            .log
            .get(start..)
            .unwrap_or_default()
            .to_vec()
    }

    pub fn log_len(&self) -> usize {
        self.0.borrow().log.len()
    }

    /// The names of every setting the fake can answer, for `--qa` queries.
    pub fn feature_names(&self) -> Vec<String> {
        self.0
            .borrow()
            .features
            .iter()
            .map(|feature| feature.name.clone())
            .collect()
    }
}

/// One `ShureDevice` handle on a [`FakeDevice`].
pub struct FakeHid {
    state: Rc<RefCell<FakeState>>,
    generation: u32,
    /// The sequence byte of the last binary packet written on this handle. The
    /// next one must follow it (wrapping); the first may be anything.
    last_seq: Cell<Option<u8>>,
}

impl FakeHid {
    fn new(state: &Rc<RefCell<FakeState>>, generation: u32) -> Self {
        Self {
            state: Rc::clone(state),
            generation,
            last_seq: Cell::new(None),
        }
    }
}

impl HidTransport for FakeHid {
    fn write(&self, packet: &[u8]) -> Result<usize> {
        let mut state = self.state.borrow_mut();
        state.handle_ok(self.generation)?;
        let seq = (state.model != DeviceModel::Mv7)
            .then(|| fake_wire::parse_request(packet))
            .flatten()
            .map(|parsed| parsed.seq);
        let expected = self.last_seq.get().map(|last| last.wrapping_add(1));
        match (seq, expected) {
            (Some(seq), Some(expected)) if seq != expected => {
                self.last_seq.set(Some(seq));
                state.refuse(packet, &format!("sequence byte {seq}, expected {expected}"));
            }
            (Some(seq), _) => {
                self.last_seq.set(Some(seq));
                state.receive(packet);
            }
            (None, _) => state.receive(packet),
        }
        Ok(packet.len())
    }

    fn read_timeout(&self, buf: &mut [u8], _timeout_ms: i32) -> Result<usize> {
        let mut state = self.state.borrow_mut();
        state.handle_ok(self.generation)?;
        let Some(report) = state.input.pop_front() else {
            return Ok(0);
        };
        let n = report.len().min(buf.len());
        buf[..n].copy_from_slice(&report[..n]);
        Ok(n)
    }

    fn serial_number(&self) -> Option<String> {
        Some(format!("FAKE-{:?}", self.state.borrow().model))
    }

    fn is_present(&self, _model: DeviceModel, _serial: &str) -> Result<bool> {
        let mut state = self.state.borrow_mut();
        if state.rebooting_polls > 0 {
            state.rebooting_polls -= 1;
            return Ok(false);
        }
        Ok(state.plugged)
    }

    fn reopen(&self, model: DeviceModel, _serial: &str) -> Result<Box<dyn HidTransport>> {
        let state = self.state.borrow();
        if !state.plugged || state.rebooting_polls > 0 {
            return Err(Disconnected(format!("{} not found", model.display_name())).into());
        }
        Ok(Box::new(FakeHid::new(&self.state, state.generation)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{EqBand, LedBehavior, ReverbType};

    const MODELS: [DeviceModel; 6] = [
        DeviceModel::Mvx2u,
        DeviceModel::Mvx2uGen2,
        DeviceModel::Mv6,
        DeviceModel::Mv6Gen2,
        DeviceModel::Mv7,
        DeviceModel::Mv7Plus,
    ];

    /// A state where every setting differs from its default, so a reply landing
    /// on the wrong field shows up. Values stay on every model's grid.
    fn distinct_state(model: DeviceModel) -> DeviceState {
        let mut state = DeviceState {
            mode: InputMode::Auto,
            auto_position: protocol::MicPosition::Far,
            auto_tone: protocol::AutoTone::Dark,
            auto_gain: protocol::AutoGain::Loud,
            gain_tenths: 300,
            muted: true,
            phantom_power: true,
            monitor_mix: 50,
            limiter_enabled: true,
            compressor: CompressorPreset::Medium,
            hpf: HpfFrequency::Hz150,
            eq_enabled: true,
            locked: true,
            denoiser_enabled: true,
            popper_stopper_enabled: false,
            mute_btn_disabled: true,
            tone: -4,
            mv6_gain_locked: true,
            adaptation_rate_fast: true,
            denoiser_level: 70,
            tone_advanced: true,
            playback_mix: 63,
            reverb_on_output: true,
            reverb_monitoring: true,
            reverb_type: ReverbType::Hall,
            reverb_intensity: 20,
            led_behavior: LedBehavior::Pulsing,
            led_solid_rgb: [1, 2, 3],
            led_pulsing_rgb: [4, 5, 6],
            led_live_edge_rgb: [7, 8, 9],
            led_live_middle_rgb: [10, 11, 12],
            led_live_interior_rgb: [13, 14, 15],
            eq_preset: protocol::EqPreset::PresenceBoost,
            led_live_meter: false,
            led_night_mode: true,
            ..FakeDevice::seed_state(model)
        };
        for (band, gain_db) in state.eq_bands.iter_mut().zip([-80, -20, 20, 40, 60]) {
            *band = EqBand {
                enabled: gain_db > 0,
                gain_db,
            };
        }
        state
    }

    /// Phase 1 gate: every GET each model's `get_state()` sends is answered,
    /// and every answer lands on the field it was seeded from.
    #[test]
    fn get_state_reads_back_the_seeded_state_on_every_model() {
        for model in MODELS {
            let seeded = distinct_state(model);
            let fake = FakeDevice::with_state(model, seeded.clone());
            let readback = fake.open().get_state().expect("readback");
            assert!(
                readback.skipped.is_empty(),
                "{model:?}: {:?}",
                readback.skipped
            );
            let state = fake.0.borrow();
            for packet in fake.log_since(0) {
                let PacketKind::Get { feature } = &packet.kind else {
                    continue;
                };
                let entry = state
                    .features
                    .iter()
                    .find(|f| &f.name == feature)
                    .unwrap_or_else(|| panic!("{model:?}: the fake can't answer {feature}"));
                let value = (entry.value)(&seeded);
                assert_eq!(
                    decode(entry, &value),
                    decode(entry, &(entry.value)(&readback.state)),
                    "{model:?} {feature}"
                );
            }
            if model == DeviceModel::Mv7 {
                assert_eq!(readback.state.muted, seeded.muted);
                assert_eq!(readback.state.gain_tenths, seeded.gain_tenths);
                assert_eq!(readback.state.mode, seeded.mode);
                assert_eq!(readback.state.monitor_mix, seeded.monitor_mix);
                assert_eq!(readback.state.eq_preset, seeded.eq_preset);
                assert_eq!(readback.state.led_night_mode, seeded.led_night_mode);
            } else {
                assert!(
                    fake.log_since(0)
                        .iter()
                        .any(|p| matches!(&p.kind, PacketKind::Get { .. }))
                );
            }
            assert_eq!(readback.state.factory_serial, "FAKE0001", "{model:?}");
        }
    }

    /// Too few acks after a CONFIRM cost a timeout but must not shift any
    /// later GET (481dc76): refresh still lands every value on its field.
    #[test]
    fn dropped_ack_leaves_readback_in_order() {
        for model in [DeviceModel::Mvx2uGen2, DeviceModel::Mv7Plus] {
            let fake = FakeDevice::new(model);
            let device = fake.open();
            fake.faults_mut(|f| f.drop_acks = 1);
            device.set_mute(true).expect("set mute");
            let readback = device.get_state().expect("readback");
            assert!(readback.state.muted, "{model:?}");
            assert_eq!(readback.state.gain_tenths, 240, "{model:?}");
            assert!(readback.skipped.is_empty(), "{model:?}");
        }
    }

    /// `send_set()` must drain every report the device sends after a CONFIRM.
    /// One left queued (a short drain, 481dc76) shifts every later GET by one,
    /// and the last reply is never read, so a readback after a SET has to
    /// match the one before it in every field but the one set. The seed holds
    /// non-default values, so a field left at its default can't pass.
    #[test]
    fn readback_after_a_set_matches_the_one_before_on_every_binary_model() {
        for model in MODELS {
            match model {
                DeviceModel::Mv7 => continue,
                DeviceModel::Mvx2u
                | DeviceModel::Mvx2uGen2
                | DeviceModel::Mv6
                | DeviceModel::Mv6Gen2
                | DeviceModel::Mv7Plus => {}
            }
            let seeded = DeviceState {
                muted: false,
                ..distinct_state(model)
            };
            let fake = FakeDevice::with_state(model, seeded);
            let device = fake.open();
            let before = device.get_state().expect("first readback").state;
            device.set_mute(true).expect("set mute");
            let after = device.get_state().expect("second readback").state;
            assert_eq!(
                after,
                DeviceState {
                    muted: true,
                    ..before
                },
                "{model:?}"
            );
            assert!(
                fake.0.borrow().input.is_empty(),
                "{model:?}: reports left queued"
            );
        }
    }

    /// Documents a known bug (481dc76's class), not intended behaviour: where
    /// the extra ack parses as a reply (the MV7+ SET echo), every GET gets an
    /// applied reply, so the readback passes. Replies route by address, so most
    /// values still land, but the last GET's reply is never read and its field
    /// keeps `DeviceState::default()`. A fix must flip these asserts, as it
    /// must the `readback-extra-ack-keeps-serial` scenario.
    #[test]
    fn extra_ack_loses_the_last_readback_field_known_bug() {
        type Field = fn(&DeviceState) -> Value;
        let cases: [(DeviceModel, &str, Field); 1] =
            [(DeviceModel::Mv7Plus, "serial", |s| json!(s.factory_serial))];
        for (model, name, field) in cases {
            let seeded = distinct_state(model);
            let fake = FakeDevice::with_state(model, seeded.clone());
            let device = fake.open();
            fake.faults_mut(|f| f.extra_acks = 1);
            device.set_mute(seeded.muted).expect("set mute");
            let state = device.get_state().expect("readback").state;
            assert_ne!(field(&seeded), field(&DeviceState::default()), "{model:?}");
            assert_eq!(
                field(&state),
                field(&DeviceState::default()),
                "{model:?}: the {name} reply was read after all"
            );
            assert_eq!(state.gain_tenths, seeded.gain_tenths, "{model:?}");
            assert_eq!(fake.0.borrow().input.len(), 1, "{model:?}");
        }
    }

    /// The acks each model sends after a CONFIRM (`fake_wire::acks_after_confirm`).
    /// The MV7+ SET echo parses as a reply, so leaving it unread misplaces a
    /// later GET's value rather than just delaying it; the Gen 2's don't parse.
    #[test]
    fn acks_after_confirm_match_the_hardware_order() {
        let mute = |model| {
            let fake = FakeDevice::new(model);
            let key = fake.0.borrow().key_of("MUTE").expect("MUTE");
            fake_wire::acks_after_confirm(model, Some((key, &[0x01])))
        };
        let parsed = |acks: &[Vec<u8>]| -> Vec<Option<protocol::Reply>> {
            acks.iter()
                .map(|ack| protocol::parse_response(ack))
                .collect()
        };
        for model in [DeviceModel::Mv7Plus, DeviceModel::Mvx2uGen2] {
            assert_eq!(
                mute(model).len(),
                model.reports_after_confirm(),
                "{model:?}"
            );
        }
        let echo = parsed(&mute(DeviceModel::Mv7Plus));
        assert_eq!(echo[0], None, "the CONFIRM ack comes first");
        let mut state = DeviceState::default();
        let reply = echo[1].as_ref().expect("the SET echo parses as a reply");
        assert!(apply_reply(reply, &mut state) && state.muted);
        assert_eq!(parsed(&mute(DeviceModel::Mvx2uGen2)), vec![None, None]);
    }

    /// Every MV7+ SET uses the 0x00 header; the same SET with the standard one
    /// matches the right address, so only the framing check catches it.
    #[test]
    fn a_set_with_the_wrong_framing_is_refused() {
        let fake = FakeDevice::new(DeviceModel::Mv7Plus);
        let transport = FakeHid::new(&fake.0, 0);
        transport
            .write(&protocol::cmd_set_mute(0, true))
            .expect("write");
        transport
            .write(&protocol::cmd_set_mv7_mute(1, true))
            .expect("write");
        let log = fake.log_since(0);
        assert!(matches!(log[0].kind, PacketKind::Refused { .. }));
        assert!(matches!(&log[1].kind, PacketKind::Set { feature, .. } if feature == "MUTE"));

        // The Mic Level SET takes the 0x00 header on every model that has it.
        let fake = FakeDevice::new(DeviceModel::Mv6);
        let device = fake.open();
        device.set_monitor_mix(30).expect("mic level");
        assert!(
            fake.log_since(0)
                .iter()
                .all(|p| !matches!(p.kind, PacketKind::Refused { .. }))
        );
    }

    #[test]
    fn a_sequence_byte_out_of_order_is_refused() {
        let fake = FakeDevice::new(DeviceModel::Mv6);
        let transport = FakeHid::new(&fake.0, 0);
        transport.write(&protocol::cmd_get_gain(7)).expect("write");
        transport.write(&protocol::cmd_get_mute(8)).expect("write");
        transport.write(&protocol::cmd_get_hpf(8)).expect("write");
        transport.write(&protocol::cmd_get_mode(9)).expect("write");
        let refused: Vec<bool> = fake
            .log_since(0)
            .iter()
            .map(|p| matches!(p.kind, PacketKind::Refused { .. }))
            .collect();
        assert_eq!(refused, vec![false, false, true, false]);
        // A new handle (a reopen) starts its own count.
        FakeHid::new(&fake.0, 0)
            .write(&protocol::cmd_get_gain(0))
            .expect("write");
        assert!(!matches!(
            fake.log_since(4)[0].kind,
            PacketKind::Refused { .. }
        ));
    }

    #[test]
    fn gain_lock_makes_the_device_ignore_gain_writes() {
        let fake = FakeDevice::new(DeviceModel::Mv6);
        let device = fake.open();
        device.set_mv6_gain_lock(true).expect("lock");
        device.set_gain(300).expect("gain");
        assert_eq!(device.get_state().expect("readback").state.gain_tenths, 240);
        let ignored = fake.log_since(0).into_iter().any(|p| {
            matches!(p.kind, PacketKind::Set { ref feature, applied: false, .. } if feature == "GAIN")
        });
        assert!(ignored);
        device.set_mv6_gain_lock(false).expect("unlock");
        device.set_gain(300).expect("gain");
        assert_eq!(device.get_state().expect("readback").state.gain_tenths, 300);
    }

    #[test]
    fn sets_are_logged_by_name_with_decoded_values() {
        let fake = FakeDevice::new(DeviceModel::Mvx2uGen2);
        let device = fake.open();
        device.set_gain(250).expect("gain");
        device.set_eq_band_gain(1, -25).expect("eq");
        let sets: Vec<Value> = fake
            .log_since(0)
            .iter()
            .filter_map(|p| match &p.kind {
                PacketKind::Set { feature, value, .. } => Some(json!([feature, value])),
                _ => None,
            })
            .collect();
        assert_eq!(
            sets,
            vec![json!(["GAIN", 250]), json!(["EQ_BAND_2_GAIN", -25])]
        );
    }

    #[test]
    fn unplugged_device_fails_io_and_is_found_again_after_replug() {
        let fake = FakeDevice::new(DeviceModel::Mv7Plus);
        let device = fake.open();
        fake.unplug();
        assert!(!device.is_present().expect("presence"));
        let err = device.set_mute(true).expect_err("unplugged");
        assert!(err.downcast_ref::<Disconnected>().is_some(), "{err}");
        assert!(device.reopen().is_err());
        fake.replug();
        let reopened = device.reopen().expect("reopen");
        assert!(device.set_mute(true).is_err(), "the old handle stays stale");
        reopened.set_mute(true).expect("new handle works");
    }

    #[test]
    fn malformed_packets_are_refused_and_logged() {
        let fake = FakeDevice::new(DeviceModel::Mv6);
        let device = fake.open();
        let mut packet = protocol::cmd_get_gain(0);
        packet[14] ^= 0xFF;
        device.set_mute(true).expect("a good packet first");
        let transport = FakeHid::new(&fake.0, 0);
        transport.write(&packet).expect("write");
        let log = fake.log_since(0);
        assert!(matches!(
            log[log.len() - 1].kind,
            PacketKind::Refused { .. }
        ));
        assert!(!matches!(log[0].kind, PacketKind::Refused { .. }));
    }

    #[test]
    fn mv7_shell_answers_gain_mute_and_mode() {
        let fake = FakeDevice::new(DeviceModel::Mv7);
        let device = fake.open();
        device.set_mute(true).expect("mute");
        device.set_gain(300).expect("gain");
        // An unchanged gain answers [Failed], which is not an error.
        device.set_gain(300).expect("same gain");
        let state = device.get_state().expect("readback").state;
        assert!(state.muted);
        assert_eq!(state.gain_tenths, 300);
        device
            .set_mode(
                true,
                protocol::MicPosition::Near,
                protocol::AutoTone::Natural,
            )
            .expect("auto");
        assert!(
            device.set_gain(150).is_err(),
            "gain is locked in Auto Level"
        );
    }

    #[test]
    fn factory_reset_reboots_and_restores_the_seed() {
        let fake = FakeDevice::new(DeviceModel::Mv6Gen2);
        let device = fake.open();
        device.set_mute(true).expect("mute");
        device.factory_reset().expect("reset");
        assert!(!device.is_present().expect("presence"), "rebooting");
        assert!(device.is_present().expect("presence"));
        let reopened = device.reopen().expect("reopen");
        assert!(!reopened.get_state().expect("readback").state.muted);
    }
}
