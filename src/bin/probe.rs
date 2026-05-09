//! MVX2U / MVX2U Gen 2 / MV6 / MV7+ HID Feature Address Probe
//!
//! Systematically sweeps unknown feature address ranges, sending CMD_GET_FEAT
//! for each address and logging every valid response. Helps discover undocumented
//! features such as native preset slots and monitor mix addresses.
//!
//! Usage:
//!   cargo run --bin probe                                    # defaults to MVX2U Gen 2 (PID 0x1033)
//!   cargo run --bin probe -- --pid 0x1013                   # target MVX2U Gen 1
//!   cargo run --bin probe -- --pid 0x1019                   # target MV7+
//!   cargo run --bin probe -- --pid 0x1026                   # target MV6
//!   cargo run --bin probe -- --output results.txt
//!   cargo run --bin probe -- --page 0x03
//!   cargo run --bin probe -- --also-mix-class          # try is_mix=0x01 for every address
//!   cargo run --bin probe -- --also-lock-class         # try CMD_GET_LOCK for every address
//!   cargo run --bin probe -- --page 0x01 --also-mix-class  # hunt for MVX2U-style mix features
//!
//! The tool is READ-ONLY — it only sends GET packets, never SET or CONFIRM.
//! It is safe to run against a live device; no settings will be changed.
//!
//! # Prefix bytes
//!
//! The payload of every GET packet starts with a prefix byte before the feature address:
//!   0x00 — standard features (used for almost everything)
//!   0x01 — mix features (used for MVX2U monitor mix at [0x01, 0x86])
//!   0x06 — lock features (used for config lock at [0x00, 0xA6])
//!
//! # MV6 monitor mix — why the probe missed it
//!
//! The MV6 monitor mix uses the same address as the MVX2U ([0x01, 0x86]). Its GET
//! packet uses standard framing (HDR_CONSTANT=0x03, prefix=0x00) — confirmed by
//! Wireshark. However, the device only responds to GET after at least one SET has
//! been issued. On a fresh device the address returns nothing, which is why the
//! probe sweep found no response.
//!
//! Its SET packet uses HDR_CONSTANT=0x00 (not the usual 0x03), which is why it
//! also didn't appear in the mix-class sweep.
//!
//! Use Wireshark (not this probe) to investigate any future MV6 features that
//! involve non-standard HDR_CONSTANT values or state-dependent GET responses.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use hidapi::{HidApi, HidDevice};

const VID: u16 = 0x14ED;
/// MVX2U Gen 1
const PID_MVX2U: u16 = 0x1013;
/// MVX2U Gen 2
const PID_MVX2U_GEN2: u16 = 0x1033;
/// MV6
const PID_MV6: u16 = 0x1026;
/// MV7+
const PID_MV7_PLUS: u16 = 0x1019;
const PACKET_SIZE: usize = 64;
const READ_TIMEOUT_MS: i32 = 150;

const REPORT_ID: u8 = 0x01;
const HEADER_MAGIC: [u8; 2] = [0x11, 0x22];
const HDR_CONSTANT: u8 = 0x03;
const HDR_END: u8 = 0x08;
const DATA_START: u8 = 0x70;

const CMD_GET_FEAT: [u8; 3] = [0x01, 0x02, 0x02];
const CMD_GET_LOCK: [u8; 3] = [0x01, 0x02, 0x01];

const RES_GET_FEAT: [u8; 3] = [0x03, 0x02, 0x02];
const RES_GET_LOCK: [u8; 3] = [0x03, 0x02, 0x01];
const RES_SET_FEAT: [u8; 3] = [0x04, 0x02, 0x02];
const RES_SET_LOCK: [u8; 3] = [0x04, 0x02, 0x01];

const KNOWN_FEATURES: &[([u8; 2], &str)] = &[
    (
        [0x00, 0xA6],
        "LOCK (Gen1 only — not present on Gen2 or MV6)",
    ),
    ([0x01, 0x02], "GAIN"),
    ([0x01, 0x04], "MUTE"),
    ([0x01, 0x06], "HPF"),
    ([0x01, 0x51], "LIMITER (Gen1/Gen2)"),
    ([0x01, 0x58], "DENOISER (MV6/Gen2)"),
    ([0x01, 0x5C], "COMPRESSOR (Gen1/Gen2)"),
    ([0x01, 0x66], "PHANTOM (Gen1/Gen2)"),
    ([0x01, 0x82], "AUTO_POSITION (Gen1 only)"),
    ([0x01, 0x83], "AUTO_TONE (Gen1 only)"),
    ([0x01, 0x85], "AUTO_LEVEL"),
    (
        [0x01, 0x86],
        "MONITOR_MIX (Gen1: GET/SET both use mix-class prefix 0x01; Gen2/MV6: GET standard, SET uses HDR_CONSTANT=0x00; device only responds to GET after first SET)",
    ),
    ([0x01, 0x87], "AUTO_GAIN (Gen1 only)"),
    ([0x01, 0xF3], "GAIN_LOCK (MV6/Gen2)"),
    (
        [0x01, 0xF4],
        "UNKNOWN_F4 (GET always returns 0x00 on MV6; not mute btn disable)",
    ),
    (
        [0x0C, 0x00],
        "MV6_MUTE_BTN_DISABLE (MV6 only — GET: CMD_GET_LOCK, payload=[0x0C,0x00,0x60], resp feat=[0x00,0x60]; SET: CMD_SET_LOCK, HDR_CONSTANT=0x00; inverted: 0x00=disabled, 0x01=active)",
    ),
    ([0x02, 0x00], "EQ_MASTER (Gen1 only — not present on Gen2)"),
    ([0x02, 0x04], "TONE_SLIDER (MV6/Gen2)"),
    (
        [0x02, 0x10],
        "EQ_100HZ_EN (Gen1 only — not present on Gen2)",
    ),
    (
        [0x02, 0x11],
        "EQ_100HZ_FREQ_RO (Gen2 only — read-only, always 100)",
    ),
    ([0x02, 0x14], "EQ_100HZ_GAIN (Gen1/Gen2)"),
    (
        [0x02, 0x20],
        "EQ_250HZ_EN (Gen1 only — not present on Gen2)",
    ),
    (
        [0x02, 0x21],
        "EQ_250HZ_FREQ_RO (Gen2 only — read-only, always 250)",
    ),
    ([0x02, 0x24], "EQ_250HZ_GAIN (Gen1/Gen2)"),
    ([0x02, 0x30], "EQ_1KHZ_EN (Gen1 only — not present on Gen2)"),
    (
        [0x02, 0x31],
        "EQ_1KHZ_FREQ_RO (Gen2 only — read-only, always 1000)",
    ),
    ([0x02, 0x34], "EQ_1KHZ_GAIN (Gen1/Gen2)"),
    ([0x02, 0x40], "EQ_4KHZ_EN (Gen1 only — not present on Gen2)"),
    (
        [0x02, 0x41],
        "EQ_4KHZ_FREQ_RO (Gen2 only — read-only, always 4000)",
    ),
    ([0x02, 0x44], "EQ_4KHZ_GAIN (Gen1/Gen2)"),
    (
        [0x02, 0x50],
        "EQ_10KHZ_EN (Gen1 only — not present on Gen2)",
    ),
    (
        [0x02, 0x51],
        "EQ_10KHZ_FREQ_RO (Gen2 only — read-only, always 10000)",
    ),
    ([0x02, 0x54], "EQ_10KHZ_GAIN (Gen1/Gen2)"),
    ([0x03, 0x81], "POPPER_STOPPER (MV6/Gen2/MV7+)"),
    ([0x03, 0x82], "MV7+_REVERB_OUTPUT (MV7+ only — 0=off, 1=on)"),
    (
        [0x03, 0x83],
        "MV7+_REVERB_TYPE (MV7+ only — 0=Plate, 1=Hall, 2=Studio)",
    ),
    ([0x03, 0x84], "MV7+_REVERB_INTENSITY (MV7+ only — 0–100)"),
    (
        [0x03, 0x85],
        "MV7+_REVERB_MONITOR (MV7+ only — 0=off, 1=on)",
    ),
];

/// Known addresses for the MV7+ lock command class (cmd=[01,02,01]).
/// These use payload [page, 0x00, sub_addr] — a 3-level address scheme not
/// covered by the standard 2-level sweep. Discovered via Wireshark captures.
/// Keys: [page, sub_addr] (the 3rd byte is the discriminator within the page).
const KNOWN_MV7_LOCK_ADDRS: &[([u8; 2], &str)] = &[
    (
        [0x0C, 0x60],
        "MV6/MV7+_MUTE_BTN_DISABLE (0x00=disabled, 0x01=active)",
    ),
    (
        [0x0C, 0xA2],
        "MV7+_LED_COLOR_ACTIVE (4 bytes: [0x00, R, G, B] — color when unmuted)",
    ),
    (
        [0x0C, 0xA3],
        "MV7+_LED_COLOR_MUTED (4 bytes: [0x00, R, G, B] — color when muted)",
    ),
    (
        [0x0C, 0xA4],
        "MV7+_LED_MODE (1 byte: 0x01=solid, 0x02=breathing)",
    ),
    (
        [0x0C, 0xA5],
        "MV7+_LED_FLAG_A5 (1 byte: always 0x01 — write-only flag)",
    ),
    (
        [0x0C, 0xA6],
        "MV7+_LED_FLAG_A6 (1 byte: always 0x01 — write-only flag; aliases FEAT_LOCK on MVX2U)",
    ),
];

#[derive(Parser)]
#[command(
    name = "probe",
    about = "Shure HID feature address probe — discovers undocumented feature addresses (READ-ONLY)"
)]
struct Cli {
    /// Target device PID in hex. Defaults to 0x1033 (MVX2U Gen 2).
    /// Use 0x1013 for MVX2U Gen 1, 0x1019 for MV7+, 0x1026 for MV6.
    #[arg(long, default_value = "0x1033")]
    pid: String,

    /// Output file for results. Defaults to probe_results.txt.
    #[arg(long, short, default_value = "probe_results.txt")]
    output: String,

    /// Only sweep a specific page (e.g. 0x03). Sweeps all pages if omitted.
    #[arg(long)]
    page: Option<String>,

    /// Also try CMD_GET_LOCK command class for every address.
    #[arg(long)]
    also_lock_class: bool,

    /// Also sweep every address with is_mix=0x01 prefix (hunts for mix-class features
    /// like the MVX2U monitor mix which requires this prefix byte).
    /// NOTE: MV6 monitor mix at [0x01, 0x86] is NOT findable this way — its SET packet
    /// uses HDR_CONSTANT=0x00 rather than a mix-class prefix. Confirmed by Wireshark
    /// capture of MOTIV app; a full sweep of all pages returned no response for this
    /// address on MV6 regardless of prefix.
    #[arg(long)]
    also_mix_class: bool,

    /// Also sweep each page using the MV7+ lock-class 3-level addressing:
    /// payload=[page, 0x00, sub_addr] with CMD_GET_LOCK. This discovers features
    /// like LED colors (page 0x0C, sub_addrs A2–A6) and mute_btn_disable (0x60)
    /// that the standard 2-level sweep cannot find. Use with --page 0x0C for MV7+.
    #[arg(long)]
    also_mv7_lock_class: bool,

    /// Delay between packets in milliseconds. Increase if the device misses responses.
    #[arg(long, default_value = "20")]
    delay_ms: u64,
}

// ── CRC-16/ANSI ───────────────────────────────────────────────────────────────
fn crc16_ansi(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x0000;
    for &byte in data {
        let mut b = byte;
        for _ in 0..8 {
            let bit = (crc ^ b as u16) & 1;
            crc >>= 1;
            if bit != 0 {
                crc ^= 0xA001;
            }
            b >>= 1;
        }
    }
    crc
}

// ── Packet builder ────────────────────────────────────────────────────────────
fn build_get_packet(seq: u8, cmd: &[u8; 3], feat_addr: [u8; 2], is_mix_or_lock: u8) -> Vec<u8> {
    let payload = [is_mix_or_lock, feat_addr[0], feat_addr[1]];
    build_get_packet_raw(seq, cmd, &payload)
}

/// Build a MV7+ lock-class GET packet using 3-level addressing: [page, 0x00, sub_addr].
/// Used for LED color features on page 0x0C and mute_btn_disable (sub_addr 0x60).
fn build_get_packet_mv7_lock(seq: u8, page: u8, sub_addr: u8) -> Vec<u8> {
    let payload = [page, 0x00u8, sub_addr];
    build_get_packet_raw(seq, &CMD_GET_LOCK, &payload)
}

fn build_get_packet_raw(seq: u8, cmd: &[u8; 3], payload: &[u8]) -> Vec<u8> {
    let data_len = (3 + payload.len() + 2) as u8;

    let mut inner: Vec<u8> = Vec::with_capacity(PACKET_SIZE);
    inner.push(HEADER_MAGIC[0]);
    inner.push(HEADER_MAGIC[1]);
    inner.push(seq);
    inner.push(HDR_CONSTANT);
    inner.push(HDR_END);
    inner.push(data_len);
    inner.push(DATA_START);
    inner.push(data_len);
    inner.extend_from_slice(cmd);
    inner.extend_from_slice(payload);

    let total_len = (inner.len() + 2) as u8;
    let crc = crc16_ansi(&inner);

    let mut pkt: Vec<u8> = Vec::with_capacity(PACKET_SIZE);
    pkt.push(REPORT_ID);
    pkt.push(total_len);
    pkt.extend_from_slice(&inner);
    pkt.push((crc >> 8) as u8);
    pkt.push((crc & 0xFF) as u8);
    pkt.resize(PACKET_SIZE, 0x00);
    pkt
}

// ── Response parser ───────────────────────────────────────────────────────────
#[derive(Debug)]
struct ParsedResponse {
    value_bytes: Vec<u8>,
    raw: Vec<u8>,
}

fn parse_response(buf: &[u8]) -> Option<ParsedResponse> {
    if buf.len() < 18 {
        return None;
    }
    let contents_end = buf[1] as usize;
    if contents_end + 2 > buf.len() {
        return None;
    }
    if buf[2] != HEADER_MAGIC[0] || buf[3] != HEADER_MAGIC[1] {
        return None;
    }
    let expected_crc = ((buf[contents_end] as u16) << 8) | buf[contents_end + 1] as u16;
    let actual_crc = crc16_ansi(&buf[2..contents_end]);
    if actual_crc != expected_crc {
        return None;
    }
    let resp_type: [u8; 3] = buf[10..13].try_into().ok()?;
    match resp_type {
        _ if resp_type == RES_GET_FEAT
            || resp_type == RES_SET_FEAT
            || resp_type == RES_GET_LOCK
            || resp_type == RES_SET_LOCK => {}
        _ => return None,
    }
    if buf.len() < 16 {
        return None;
    }
    let value_bytes = buf[16..contents_end].to_vec();
    Some(ParsedResponse {
        value_bytes,
        raw: buf.to_vec(),
    })
}

// ── Hex dump ──────────────────────────────────────────────────────────────────
fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "  {:04x}:  ", i * 16);
        for b in chunk {
            let _ = write!(out, "{b:02x} ");
        }
        for _ in chunk.len()..16 {
            out.push_str("   ");
        }
        out.push_str("  |");
        for &b in chunk {
            let ch = if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            };
            out.push(ch);
        }
        out.push_str("|\n");
    }
    out
}

fn fmt_value_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn known_name(addr: [u8; 2]) -> Option<&'static str> {
    KNOWN_FEATURES
        .iter()
        .find(|(a, _)| *a == addr)
        .map(|(_, name)| *name)
}

fn known_mv7_lock_name(page: u8, sub_addr: u8) -> Option<&'static str> {
    KNOWN_MV7_LOCK_ADDRS
        .iter()
        .find(|(a, _)| *a == [page, sub_addr])
        .map(|(_, name)| *name)
}

// ── Probe logic ───────────────────────────────────────────────────────────────
struct Probe {
    device: HidDevice,
    seq: u8,
    delay: Duration,
    output_log: String,
    hits: Vec<ProbeHit>,
}

#[derive(Debug)]
struct ProbeHit {
    addr: [u8; 2],
    value_bytes: Vec<u8>,
    prefix: u8,
    cmd_class: &'static str,
    is_known: bool,
    known_name: Option<&'static str>,
}

impl Probe {
    fn new(device: HidDevice, delay_ms: u64) -> Self {
        Self {
            device,
            seq: 0,
            delay: Duration::from_millis(delay_ms),
            output_log: String::new(),
            hits: Vec::new(),
        }
    }

    fn next_seq(&mut self) -> u8 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    fn log(&mut self, line: &str) {
        println!("{line}");
        self.output_log.push_str(line);
        self.output_log.push('\n');
    }

    fn probe_address(
        &mut self,
        cmd: &[u8; 3],
        addr: [u8; 2],
        is_mix_or_lock: u8,
    ) -> Result<Option<ParsedResponse>> {
        let seq = self.next_seq();
        let pkt = build_get_packet(seq, cmd, addr, is_mix_or_lock);
        self.device.write(&pkt).context("HID write failed")?;
        std::thread::sleep(self.delay);
        let mut buf = vec![0u8; PACKET_SIZE];
        match self.device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(parse_response(&buf[..n])),
            Err(e) => Err(anyhow!("HID read failed: {e}")),
        }
    }

    /// Sweep all 256 addresses on a page using CMD_GET_FEAT with is_mix=0x00.
    fn sweep_page(&mut self, page: u8) -> Result<()> {
        self.log(&format!(
            "\n── Page 0x{page:02X} sweep (CMD_GET_FEAT, prefix=0x00) ─────────────────"
        ));
        self.sweep_page_with_prefix(page, &CMD_GET_FEAT, 0x00, "standard")
    }

    /// Sweep all 256 addresses on a page using CMD_GET_FEAT with is_mix=0x01.
    /// This catches mix-class features that require the 0x01 prefix byte,
    /// like the MVX2U monitor mix and potentially the MV6 monitor level sliders.
    fn sweep_page_mix_class(&mut self, page: u8) -> Result<()> {
        self.log(&format!(
            "\n── Page 0x{page:02X} sweep (CMD_GET_FEAT, prefix=0x01 / mix class) ─────"
        ));
        self.sweep_page_with_prefix(page, &CMD_GET_FEAT, 0x01, "mix")
    }

    /// Sweep all 256 addresses on a page using CMD_GET_LOCK command class.
    fn sweep_page_lock_class(&mut self, page: u8) -> Result<()> {
        self.log(&format!(
            "\n── Page 0x{page:02X} sweep (CMD_GET_LOCK class, prefix=0x06) ────────────"
        ));
        self.sweep_page_with_prefix(page, &CMD_GET_LOCK, 0x06, "lock")
    }

    /// Sweep all 256 sub-addresses on a page using MV7+ 3-level addressing:
    /// payload=[page, 0x00, sub_addr] with CMD_GET_LOCK.
    ///
    /// This discovers features like LED colors ([0x0C, 0x00, 0xA2..0xA6]) and
    /// mute_btn_disable ([0x0C, 0x00, 0x60]) that the standard 2-level sweep
    /// cannot find because they use a different payload structure.
    ///
    /// To probe MV7+ LED: cargo run --bin probe -- --pid 0x1019 --page 0x0C --also-mv7-lock-class
    fn sweep_page_mv7_lock(&mut self, page: u8) -> Result<()> {
        self.log(&format!(
            "\n── Page 0x{page:02X} sweep (MV7+ lock class: [page, 0x00, sub_addr]) ────"
        ));
        let mut responded = 0u32;
        let mut new_found = 0u32;

        for sub_addr in 0x00u8..=0xFF {
            let seq = self.next_seq();
            let pkt = build_get_packet_mv7_lock(seq, page, sub_addr);
            self.device.write(&pkt).context("HID write failed")?;
            std::thread::sleep(self.delay);
            let mut buf = vec![0u8; PACKET_SIZE];
            let resp = match self.device.read_timeout(&mut buf, READ_TIMEOUT_MS) {
                Ok(0) => None,
                Ok(n) => parse_response(&buf[..n]),
                Err(e) => return Err(anyhow!("HID read failed: {e}")),
            };
            if let Some(resp) = resp {
                responded += 1;
                let is_known = known_mv7_lock_name(page, sub_addr).is_some();
                if !is_known {
                    new_found += 1;
                }
                let name_tag = known_mv7_lock_name(page, sub_addr)
                    .map(|n| format!(" [{n}]"))
                    .unwrap_or_else(|| " *** NEW ***".to_string());
                let val_hex = fmt_value_hex(&resp.value_bytes);
                self.log(&format!(
                    "  RESP  [{page:02X} 00 {sub_addr:02X}]{name_tag}  value: [{val_hex}]  ({} bytes)",
                    resp.value_bytes.len(),
                ));
                if !is_known {
                    self.log("  Raw response packet:");
                    self.log(&hex_dump(&resp.raw));
                }
                self.hits.push(ProbeHit {
                    addr: [page, sub_addr],
                    value_bytes: resp.value_bytes,
                    prefix: 0x00,
                    cmd_class: "mv7-lock",
                    is_known,
                    known_name: known_mv7_lock_name(page, sub_addr),
                });
            }
        }

        self.log(&format!(
            "  Page 0x{page:02X} (mv7-lock): {responded} addresses responded, {new_found} previously unknown."
        ));
        Ok(())
    }

    fn sweep_page_with_prefix(
        &mut self,
        page: u8,
        cmd: &[u8; 3],
        prefix: u8,
        class_label: &'static str,
    ) -> Result<()> {
        let mut responded = 0u32;
        let mut new_found = 0u32;

        for addr_lo in 0x00u8..=0xFF {
            let addr = [page, addr_lo];

            // For the standard sweep, use the known prefix for already-identified
            // mix-class addresses so we don't miss them.
            let effective_prefix = if class_label == "standard" && addr == [0x01, 0x86] {
                0x01
            } else {
                prefix
            };

            match self.probe_address(cmd, addr, effective_prefix) {
                Ok(Some(resp)) => {
                    responded += 1;
                    let is_known = known_name(addr).is_some();
                    if !is_known {
                        new_found += 1;
                    }
                    let name_tag = known_name(addr)
                        .map(|n| format!(" [{n}]"))
                        .unwrap_or_else(|| " *** NEW ***".to_string());
                    let val_hex = fmt_value_hex(&resp.value_bytes);
                    self.log(&format!(
                        "  RESP  [{:02X} {:02X}]{}  prefix=0x{effective_prefix:02X}  value: [{}]  ({} bytes)",
                        addr[0], addr[1], name_tag, val_hex, resp.value_bytes.len(),
                    ));
                    if !is_known {
                        self.log("  Raw response packet:");
                        self.log(&hex_dump(&resp.raw));
                    }
                    self.hits.push(ProbeHit {
                        addr,
                        value_bytes: resp.value_bytes,
                        prefix: effective_prefix,
                        cmd_class: class_label,
                        is_known,
                        known_name: known_name(addr),
                    });
                }
                Ok(None) => {}
                Err(e) => {
                    self.log(&format!("  ERROR [{:02X} {:02X}]: {e}", addr[0], addr[1]));
                    return Err(e);
                }
            }
        }

        self.log(&format!(
            "  Page 0x{page:02X} ({class_label}): {responded} addresses responded, {new_found} previously unknown."
        ));
        Ok(())
    }

    fn print_summary(&mut self) {
        let known_lines: Vec<String> = self
            .hits
            .iter()
            .filter(|h| h.is_known)
            .map(|h| {
                format!(
                    "  [{:02X} {:02X}]  {:32}  prefix=0x{:02X}  class={}  value: [{}]",
                    h.addr[0],
                    h.addr[1],
                    h.known_name.unwrap_or("?"),
                    h.prefix,
                    h.cmd_class,
                    fmt_value_hex(&h.value_bytes),
                )
            })
            .collect();

        let new_lines: Vec<String> = self
            .hits
            .iter()
            .filter(|h| !h.is_known)
            .map(|h| {
                format!(
                    "  [{:02X} {:02X}]  *** UNKNOWN ***  prefix=0x{:02X}  class={}  value: [{}]  {} bytes",
                    h.addr[0],
                    h.addr[1],
                    h.prefix,
                    h.cmd_class,
                    fmt_value_hex(&h.value_bytes),
                    h.value_bytes.len(),
                )
            })
            .collect();

        self.log("\n══════════════════════════════════════════════════════════════════════");
        self.log("SUMMARY — all responding addresses");
        self.log("══════════════════════════════════════════════════════════════════════");

        self.log(&format!(
            "\nKnown addresses that responded: {}",
            known_lines.len()
        ));
        for line in &known_lines {
            self.log(line);
        }

        self.log(&format!(
            "\nNEW / UNKNOWN addresses that responded: {}",
            new_lines.len()
        ));
        if new_lines.is_empty() {
            self.log("  (none found — all responsive addresses are already known)");
        } else {
            for line in &new_lines {
                self.log(line);
            }
            self.log("\n  ACTION: Add these addresses to protocol.rs as FEAT_* constants.");
        }

        self.log("\n══════════════════════════════════════════════════════════════════════\n");
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let pid: u16 = {
        let s = cli.pid.trim_start_matches("0x").trim_start_matches("0X");
        u16::from_str_radix(s, 16).context("--pid must be a hex value, e.g. 0x1033")?
    };

    let device_label = match pid {
        PID_MVX2U => "MVX2U Gen 1",
        PID_MVX2U_GEN2 => "MVX2U Gen 2",
        PID_MV6 => "MV6",
        PID_MV7_PLUS => "MV7+",
        _ => "Unknown",
    };

    let specific_page: Option<u8> = cli
        .page
        .as_deref()
        .map(|s| {
            let s = s.trim_start_matches("0x").trim_start_matches("0X");
            u8::from_str_radix(s, 16).context("--page must be a hex byte, e.g. 0x03 or 03")
        })
        .transpose()?;

    let api = HidApi::new().context("Failed to initialise hidapi")?;
    let device = api.open(VID, pid).map_err(|e| {
        anyhow!(
            "Cannot open device (VID={:#06x} PID={:#06x} / {}): {e}\n\
            Hint: check udev rules and group membership, or run with sudo.",
            VID,
            pid,
            device_label,
        )
    })?;
    device.set_blocking_mode(false).ok();

    let serial = device
        .get_serial_number_string()
        .ok()
        .flatten()
        .unwrap_or_else(|| "(unknown)".to_string());
    let product = device
        .get_product_string()
        .ok()
        .flatten()
        .unwrap_or_else(|| "(unknown)".to_string());

    let mut probe = Probe::new(device, cli.delay_ms);

    let start_time = chrono_now();
    probe.log("══════════════════════════════════════════════════════════════════════");
    probe.log(&format!("Shure HID Feature Address Probe  —  {start_time}"));
    probe.log("══════════════════════════════════════════════════════════════════════");
    probe.log(&format!("Device : {product} ({device_label})"));
    probe.log(&format!("PID    : {pid:#06x}"));
    probe.log(&format!("Serial : {serial}"));
    probe.log(&format!("Output : {}", cli.output));
    probe.log(&format!("Delay  : {} ms between packets", cli.delay_ms));
    probe.log(&format!("Mix class sweep     : {}", cli.also_mix_class));
    probe.log(&format!("Lock class sweep    : {}", cli.also_lock_class));
    probe.log(&format!(
        "MV7+ lock class     : {}",
        cli.also_mv7_lock_class
    ));
    probe.log("Note   : READ-ONLY — only GET packets are sent. No settings changed.");

    let pages_to_sweep: Vec<u8> = match specific_page {
        Some(p) => vec![p],
        None => vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
    };

    let start = Instant::now();

    for page in &pages_to_sweep {
        probe.sweep_page(*page)?;
        if cli.also_mix_class {
            probe.sweep_page_mix_class(*page)?;
        }
        if cli.also_lock_class {
            probe.sweep_page_lock_class(*page)?;
        }
        if cli.also_mv7_lock_class {
            probe.sweep_page_mv7_lock(*page)?;
        }
    }

    let elapsed = start.elapsed();
    probe.log(&format!(
        "\nSweep completed in {:.1}s",
        elapsed.as_secs_f64()
    ));

    probe.print_summary();

    fs::write(&cli.output, &probe.output_log)
        .with_context(|| format!("Failed to write results to '{}'", cli.output))?;

    println!("\nResults written to: {}", cli.output);
    Ok(())
}

fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let year = 1970 + days / 365;
    let day_of_year = days % 365;
    let month = day_of_year / 30 + 1;
    let day = day_of_year % 30 + 1;
    format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02} UTC")
}
