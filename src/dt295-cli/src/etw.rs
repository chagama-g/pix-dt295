//! `dt295 etw-urbs`: turn a USB ETW trace of the vendor driver into a PIX-DT295 access log.
//!
//! The trace comes from `tools/usb_etw_capture.cmd`, decoded with
//! `tracerpt usbtrace.etl -o usbtrace.xml -of XML -y`. The UCX provider logs every URB's
//! setup packet (vendor requests: bRequest, wValue, wIndex, wLength) and the bulk
//! lengths, but no data payloads, so register values and doorbell bytes are not
//! recoverable -- only the order and the addresses of the accesses.
//!
//! Output (TSV, one line per access; consecutive 64-byte block chunks merged):
//! ```text
//! t  R|W|M addr len        vendor 0xBC read / write / masked write
//! t  DB ch0|ch1 len done   doorbell 0xB8 / 0xB9
//! t  VREQ bm/req wValue/wIndex/wLength done
//! t  BULK ep requested done t_done
//! ```
//! `done` is the completed length (`-` if no completion was logged). The interrupt
//! endpoint 0x83 is left out (it is polled continuously).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

const UCX: &str = "Microsoft-Windows-USB-UCX";
/// Only the MB86M01 interface has this endpoint: it identifies the DT295 in the trace.
const EP_DT295: u64 = 0x89;
const EP_INT: u64 = 0x83;

/// `dt295 etw-urbs` arguments.
#[derive(Debug, clap::Args)]
pub struct EtwArgs {
    /// The trace decoded to XML by tracerpt.
    pub xml: PathBuf,
    /// The access log to write (TSV).
    pub tsv: PathBuf,
}

pub fn run(args: &EtwArgs) -> anyhow::Result<()> {
    let file = File::open(&args.xml).with_context(|| args.xml.display().to_string())?;
    let urbs = read_trace(BufReader::with_capacity(1 << 20, file))
        .with_context(|| args.xml.display().to_string())?;
    let Some(dev) = urbs.device.as_deref() else {
        bail!("no PIX-DT295 in the trace (no UCX endpoint 0x89)");
    };
    let lines = access_log(&urbs.rows);
    write_lines(&args.tsv, &lines).with_context(|| args.tsv.display().to_string())?;
    println!(
        "device {dev}: {} URBs -> {} lines",
        urbs.rows.len(),
        lines.len()
    );
    Ok(())
}

fn write_lines(path: &Path, lines: &[String]) -> std::io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    for l in lines {
        writeln!(w, "{l}")?;
    }
    w.flush()
}

/// One URB the vendor driver sent to the DT295.
#[derive(Debug, Clone, Default, PartialEq)]
struct Urb {
    /// Seconds since the first DT295 event.
    t: f64,
    /// 26 = bulk / interrupt dispatch, 56 = vendor-request dispatch.
    eid: u32,
    ep: Option<u64>,
    req: u64,
    done: Option<u64>,
    t_done: Option<f64>,
    bm_request_type: u64,
    b_request: u64,
    w_value: u64,
    w_index: u64,
    w_length: u64,
}

#[derive(Debug, Default)]
struct Trace {
    device: Option<String>,
    rows: Vec<Urb>,
}

/// One `<Event>`: provider, id, time and the first value of each `<Data Name=...>`.
#[derive(Debug, Default)]
struct EventRec {
    provider: Option<String>,
    id: Option<u32>,
    time: Option<String>,
    data: HashMap<String, Option<String>>,
}

impl EventRec {
    fn get(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(|v| v.as_deref())
    }

    fn hex(&self, key: &str) -> u64 {
        parse_hex(self.get(key))
    }
}

/// A hex field as the trace writes it (`0x...`); missing or empty = 0.
fn parse_hex(v: Option<&str>) -> u64 {
    let s = v.unwrap_or("").trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if s.is_empty() {
        0
    } else {
        u64::from_str_radix(s, 16).unwrap_or(0)
    }
}

/// Seconds within the day of an ETW `SystemTime` (`...THH:MM:SS.fffffff...`).
fn seconds(system_time: &str) -> Option<f64> {
    let (_, clock) = system_time.split_once('T')?;
    let mut it = clock.splitn(3, ':');
    let h: u64 = it.next()?.parse().ok()?;
    let m: u64 = it.next()?.parse().ok()?;
    let rest = it.next()?;
    let (s, frac) = rest.split_once('.')?;
    let s: u64 = s.parse().ok()?;
    let digits: String = frac.chars().take_while(char::is_ascii_digit).collect();
    let f: u64 = digits.parse().ok()?;
    let scale = 10f64.powi(i32::try_from(digits.len()).ok()?);
    Some((h * 3600 + m * 60 + s) as f64 + f as f64 / scale)
}

fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// Read the UCX events of the trace and keep the URBs of the DT295.
fn read_trace(src: impl std::io::BufRead) -> anyhow::Result<Trace> {
    let mut reader = Reader::from_reader(src);
    let mut buf = Vec::new();
    let mut ev: Option<EventRec> = None;
    // the element whose text is being collected: EventID or Data(name)
    let mut text_for: Option<Option<String>> = None;
    let mut text = String::new();
    let mut sink = Collector::default();
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(e) => match e.local_name().as_ref() {
                "Event" => ev = Some(EventRec::default()),
                "EventID" if ev.is_some() => {
                    text_for = Some(None);
                    text.clear();
                }
                "Data" if ev.is_some() => {
                    text_for = Some(Some(attr(&e, "Name").unwrap_or_default()));
                    text.clear();
                }
                name => start_attrs(ev.as_mut(), name, &e),
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                "Data" => {
                    if let Some(r) = ev.as_mut() {
                        let key = attr(&e, "Name").unwrap_or_default();
                        r.data.entry(key).or_insert(None);
                    }
                }
                name => start_attrs(ev.as_mut(), name, &e),
            },
            Event::Text(t) => {
                if text_for.is_some() {
                    text.push_str(&t.xml10_content());
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "EventID" => {
                    if let (Some(r), Some(None)) = (ev.as_mut(), &text_for) {
                        r.id = text.trim().parse().ok();
                    }
                    text_for = None;
                }
                "Data" => {
                    if let (Some(r), Some(Some(key))) = (ev.as_mut(), text_for.take()) {
                        r.data.entry(key).or_insert(Some(text.clone()));
                    }
                }
                "Event" => {
                    if let Some(r) = ev.take() {
                        sink.event(&r);
                    }
                }
                _ => {}
            },
            _ => {}
        }
        buf.clear();
    }
    Ok(Trace {
        device: sink.device,
        rows: sink.rows,
    })
}

/// The attributes the event header carries: Provider Name, TimeCreated SystemTime.
fn start_attrs(ev: Option<&mut EventRec>, name: &str, e: &BytesStart<'_>) {
    let Some(r) = ev else { return };
    match name {
        "Provider" if r.provider.is_none() => r.provider = attr(e, "Name"),
        "TimeCreated" if r.time.is_none() => r.time = attr(e, "SystemTime"),
        _ => {}
    }
}

#[derive(Debug, Default)]
struct Collector {
    /// pipe handle -> endpoint address
    ep_of: HashMap<String, u64>,
    device: Option<String>,
    t0: Option<f64>,
    /// URB pointer -> index into `rows`
    pending: HashMap<String, usize>,
    rows: Vec<Urb>,
}

impl Collector {
    fn event(&mut self, r: &EventRec) {
        if r.provider.as_deref() != Some(UCX) {
            return;
        }
        let Some(eid) = r.id else { return };
        let Some(t) = r.time.as_deref().and_then(seconds) else {
            return;
        };
        if eid == 6 || eid == 16 {
            // endpoint rundown / create: map the pipe to its endpoint
            let ep = r.hex("fid_bEndpointAddress");
            if let Some(pipe) = r.get("fid_PipeHandle") {
                self.ep_of.insert(pipe.to_uppercase(), ep);
            }
            if ep == EP_DT295
                && let Some(dev) = r.get("fid_UsbDevice")
            {
                self.device = Some(dev.to_uppercase());
            }
            return;
        }
        let Some(dev) = self.device.as_deref() else {
            return;
        };
        if r.get("fid_UsbDevice").unwrap_or("").to_uppercase() != dev {
            return;
        }
        let t0 = *self.t0.get_or_insert(t);
        let urb = r.get("fid_URB_Ptr").unwrap_or("").to_owned();
        match eid {
            21 | 27 => {
                // completion
                if let Some(i) = self.pending.remove(&urb) {
                    self.rows[i].done = Some(r.hex("fid_URB_TransferBufferLength"));
                    self.rows[i].t_done = Some(t - t0);
                }
            }
            26 | 56 => {
                // bulk / interrupt dispatch, vendor-request dispatch
                let ep = r
                    .get("fid_PipeHandle")
                    .and_then(|p| self.ep_of.get(&p.to_uppercase()).copied());
                self.pending.insert(urb, self.rows.len());
                self.rows.push(Urb {
                    t: t - t0,
                    eid,
                    ep,
                    req: r.hex("fid_URB_TransferBufferLength"),
                    bm_request_type: r.hex("fid_URB_Setup_bmRequestType"),
                    b_request: r.hex("fid_URB_Setup_bRequest"),
                    w_value: r.hex("fid_URB_Setup_wValue"),
                    w_index: r.hex("fid_URB_Setup_wIndex"),
                    w_length: r.hex("fid_URB_Setup_wLength"),
                    ..Urb::default()
                });
            }
            _ => {}
        }
    }
}

fn opt(v: Option<u64>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| v.to_string())
}

/// A run of 0xBC accesses being merged: (t, kind, total length, first length, next address, address).
struct Block {
    t: f64,
    kind: char,
    len: u64,
    first: u64,
    next: u64,
    addr: u64,
}

impl Block {
    fn line(&self) -> String {
        format!(
            "{:.6}\t{}\t{:06x}\t{}",
            self.t, self.kind, self.addr, self.len
        )
    }
}

/// The access log lines of the URBs.
fn access_log(rows: &[Urb]) -> Vec<String> {
    let mut out = Vec::new();
    let mut last: Option<Block> = None;
    let flush = |last: &mut Option<Block>, out: &mut Vec<String>| {
        if let Some(b) = last.take() {
            out.push(b.line());
        }
    };
    for r in rows {
        if r.eid == 26 {
            flush(&mut last, &mut out);
            if r.ep != Some(EP_INT) {
                let ep = r.ep.map_or_else(|| "-1".to_owned(), |e| format!("{e:02x}"));
                let t_done = r
                    .t_done
                    .map_or_else(|| "-".to_owned(), |t| format!("{t:.6}"));
                out.push(format!(
                    "{:.6}\tBULK\t{ep}\t{}\t{}\t{t_done}",
                    r.t,
                    r.req,
                    opt(r.done)
                ));
            }
            continue;
        }
        let (br, wv, wl) = (r.b_request, r.w_value, r.w_length);
        if br == 0xB8 || br == 0xB9 {
            flush(&mut last, &mut out);
            out.push(format!(
                "{:.6}\tDB\tch{}\t{wl}\t{}\t",
                r.t,
                br - 0xB8,
                opt(r.done)
            ));
            continue;
        }
        if br != 0xBC {
            flush(&mut last, &mut out);
            out.push(format!(
                "{:.6}\tVREQ\t{:02x}/{br:02x}\t{wv:04x}/{:04x}/{wl}\t{}\t",
                r.t,
                r.bm_request_type,
                r.w_index,
                opt(r.done)
            ));
            continue;
        }
        let addr = ((wv >> 8) << 16) | r.w_index;
        let kind = if wv & 1 != 0 {
            'M'
        } else if r.bm_request_type & 0x80 != 0 {
            'R'
        } else {
            'W'
        };
        if let Some(b) = last.as_mut()
            && b.kind == kind
            && kind != 'M'
            && wl == 64
            && b.first == 64
            && b.next == addr
        {
            b.len += wl;
            b.next = addr + wl;
            continue;
        }
        flush(&mut last, &mut out);
        last = Some(Block {
            t: r.t,
            kind,
            len: wl,
            first: wl,
            next: addr + wl,
            addr,
        });
    }
    flush(&mut last, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(eid: u32, time: &str, data: &[(&str, &str)]) -> String {
        let mut s = format!(
            "<Event xmlns=\"http://schemas.microsoft.com/win/2004/08/events/event\"><System>\
             <Provider Name=\"{UCX}\" Guid=\"{{x}}\" /><EventID>{eid}</EventID>\
             <TimeCreated SystemTime=\"2026-10-16T12:44:{time}+08:59\" /></System><EventData>"
        );
        for (k, v) in data {
            s += &format!("<Data Name=\"{k}\">{v}</Data>");
        }
        s += "</EventData><RenderingInfo Culture=\"ja-JP\"><Provider>UCX</Provider></RenderingInfo></Event>";
        s
    }

    fn vendor(time: &str, urb: &str, bm: u8, req: u8, wv: u16, wi: u16, wl: u16) -> String {
        event(
            56,
            time,
            &[
                ("fid_UsbDevice", "0xdev1"),
                ("fid_URB_Ptr", urb),
                ("fid_PipeHandle", "0xp0"),
                ("fid_URB_TransferBufferLength", &format!("0x{wl:x}")),
                ("fid_URB_Setup_bmRequestType", &format!("0x{bm:x}")),
                ("fid_URB_Setup_bRequest", &format!("0x{req:x}")),
                ("fid_URB_Setup_wValue", &format!("0x{wv:x}")),
                ("fid_URB_Setup_wIndex", &format!("0x{wi:x}")),
                ("fid_URB_Setup_wLength", &format!("0x{wl:x}")),
            ],
        )
    }

    fn trace(events: &[String]) -> Trace {
        let xml = format!("<Events>{}</Events>", events.concat());
        read_trace(xml.as_bytes()).unwrap()
    }

    #[test]
    fn times() {
        assert_eq!(
            seconds("2026-10-16T12:44:46.100079500+08:59"),
            Some(45886.1000795)
        );
        assert_eq!(seconds("2026-10-16T00:00:01.5Z"), Some(1.5));
        assert_eq!(seconds("garbage"), None);
    }

    #[test]
    fn log_lines() {
        let rundown = |pipe: &str, ep: &str, dev: &str| {
            event(
                6,
                "40.000000000",
                &[
                    ("fid_PipeHandle", pipe),
                    ("fid_bEndpointAddress", ep),
                    ("fid_UsbDevice", dev),
                ],
            )
        };
        let t = trace(&[
            rundown("0xp81", "0x81", "0xdev1"),
            rundown("0xp83", "0x83", "0xdev1"),
            rundown("0xp89", "0x89", "0xdev1"),
            // another device: ignored
            vendor("40.500000000", "0xu0", 0xC0, 0xBC, 0x0800, 0x2008, 2).replace("dev1", "dev2"),
            // two 64-byte block chunks merge, a masked write does not
            vendor("41.000000000", "0xu1", 0x40, 0xBC, 0x0800, 0x00C0, 64),
            vendor("41.000100000", "0xu2", 0x40, 0xBC, 0x0800, 0x0100, 64),
            vendor("41.000200000", "0xu3", 0x40, 0xBC, 0x0901, 0x0074, 8),
            // a doorbell and its completion
            vendor("41.001000000", "0xu4", 0x40, 0xB9, 0, 0, 4),
            event(
                27,
                "41.001500000",
                &[
                    ("fid_UsbDevice", "0xDEV1"),
                    ("fid_URB_Ptr", "0xu4"),
                    ("fid_URB_TransferBufferLength", "0x4"),
                ],
            ),
            // bulk on EP 0x81 (no completion), interrupt EP 0x83 left out
            event(
                26,
                "41.002000000",
                &[
                    ("fid_UsbDevice", "0xdev1"),
                    ("fid_URB_Ptr", "0xu5"),
                    ("fid_PipeHandle", "0xP81"),
                    ("fid_URB_TransferBufferLength", "0x17800"),
                ],
            ),
            event(
                26,
                "41.003000000",
                &[
                    ("fid_UsbDevice", "0xdev1"),
                    ("fid_URB_Ptr", "0xu6"),
                    ("fid_PipeHandle", "0xp83"),
                    ("fid_URB_TransferBufferLength", "0x6"),
                ],
            ),
            vendor("41.004000000", "0xu7", 0xC0, 0x99, 0x1234, 0x0005, 2),
        ]);
        assert_eq!(t.device.as_deref(), Some("0XDEV1"));
        assert_eq!(t.rows.len(), 7);
        assert_eq!(
            access_log(&t.rows),
            [
                "0.000000\tW\t0800c0\t128",
                "0.000200\tM\t090074\t8",
                "0.001000\tDB\tch1\t4\t4\t",
                "0.002000\tBULK\t81\t96256\t-\t-",
                "0.004000\tVREQ\tc0/99\t1234/0005/2\t-\t",
            ]
        );
    }

    #[test]
    fn hex_fields() {
        assert_eq!(parse_hex(Some(" 0x1F ")), 31);
        assert_eq!(parse_hex(Some("ff")), 255);
        assert_eq!(parse_hex(None), 0);
        assert_eq!(parse_hex(Some("")), 0);
    }
}
