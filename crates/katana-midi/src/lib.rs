//! KATANA MkII device actor.
//!
//! One thread owns the MIDI ports (ALSA on Linux, WinMM on Windows). The UI sends [`Cmd`]s, the actor emits [`Event`]s through
//! a callback. Incoming MIDI arrives on the same channel as commands, so the thread sleeps
//! in `recv()` when idle — no polling, no timers.

use katana_core::patch::Patch;
use katana_core::sysex::{self, SLOT_COUNT};
use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::collections::VecDeque;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum Cmd {
    Connect,
    /// Load a patch into the amp's temporary area (instant audition; nothing stored).
    Audition(Box<Patch>),
    /// Set raw bytes inside the temporary patch (relative linear address).
    Param { rel: u32, data: Vec<u8> },
    /// Store a patch in a user slot (1..=8), then verify by reading it back.
    WriteSlot { slot: u8, patch: Box<Patch> },
    ReadSlot(u8),
    ReadAllSlots,
    ReadTemp,
    SelectSlot(u8),
    /// A MIDI device node appeared or vanished (from the hotplug watcher).
    Hotplug,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Event {
    Connected { port: String },
    Disconnected(String),
    SlotNames(Vec<String>),
    CurrentSlot(u8),
    SlotPatch(u8, Box<Patch>),
    AllSlots(Vec<Patch>),
    TempPatch(Box<Patch>),
    /// The player turned a knob on the amp (temporary area changed).
    TempEdited { rel: u32, data: Vec<u8> },
    Progress { label: String, done: usize, total: usize },
    Written { slot: u8, verified: bool },
    Error(String),
}

enum Msg {
    Cmd(Cmd),
    Midi(Vec<u8>),
}

#[derive(Clone)]
pub struct Device {
    tx: Sender<Msg>,
}

impl Device {
    pub fn spawn(emit: impl Fn(Event) + Send + 'static) -> Device {
        let (tx, rx) = channel();
        let tx2 = tx.clone();
        watch_hotplug(tx.clone());
        thread::Builder::new()
            .name("katana-midi".into())
            .spawn(move || Actor { rx, tx: tx2, emit: Box::new(emit), conn: None, pending: VecDeque::new() }.run())
            .expect("spawn midi thread");
        Device { tx }
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.tx.send(Msg::Cmd(c));
    }
}

struct Conn {
    out: MidiOutputConnection,
    _inp: MidiInputConnection<()>,
    dev: u8,
}

struct Actor {
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    emit: Box<dyn Fn(Event) + Send>,
    conn: Option<Conn>,
    pending: VecDeque<Cmd>,
}

const RQ1_TIMEOUT: Duration = Duration::from_millis(800);
/// Gap between bulk DT1 messages. BTS uses ~10 ms; the amp drops data when flooded.
const DT1_GAP: Duration = Duration::from_millis(10);
/// Time the amp needs to reload a block's defaults after a TYPE change.
const TYPE_RELOAD: Duration = Duration::from_millis(80);

impl Actor {
    fn run(mut self) {
        loop {
            let msg = match self.pending.pop_front() {
                Some(c) => Msg::Cmd(c),
                None => match self.rx.recv() {
                    Ok(m) => m,
                    Err(_) => return,
                },
            };
            match msg {
                Msg::Midi(m) => self.on_unsolicited(&m),
                Msg::Cmd(Cmd::Shutdown) => {
                    if self.conn.is_some() {
                        let _ = self.dt1(sysex::EDITOR_COMMUNICATION_MODE, &[0]);
                    }
                    return;
                }
                Msg::Cmd(c) => {
                    if let Err(e) = self.handle(c) {
                        (self.emit)(Event::Error(e));
                    }
                }
            }
        }
    }

    fn handle(&mut self, c: Cmd) -> Result<(), String> {
        if !matches!(c, Cmd::Connect | Cmd::Hotplug) && self.conn.is_none() {
            return Err("Amp not connected".into());
        }
        match c {
            Cmd::Connect => self.connect(false),
            Cmd::Hotplug => {
                // Re-scan: picks up a freshly plugged amp, notices a vanished one. Quiet when
                // some other MIDI device came or went and nothing changed for us.
                let was = self.conn.is_some();
                if was && self.raw_send(&sysex::IDENTITY_REQUEST).is_ok()
                    && self.wait_midi(Duration::from_millis(400), sysex::parse_identity).is_some()
                {
                    return Ok(());
                }
                self.connect(!was)
            }
            Cmd::Audition(mut p) => {
                // Clicking through the library queues auditions; only the newest one matters.
                while let Ok(m) = self.rx.try_recv() {
                    match m {
                        Msg::Cmd(Cmd::Audition(q)) => p = q,
                        Msg::Cmd(other) => self.pending.push_back(other),
                        Msg::Midi(m) => self.on_unsolicited(&m),
                    }
                }
                // A new AMP / FX / delay TYPE makes the amp reload that block's defaults right
                // after the write, clobbering params that arrived in the same burst (measured on
                // hardware: DRIVE, FX rates, levels came back from the previous tone). A second
                // pass after the reload lands every value. Status carries the panel LED colours.
                let msgs = p.dt1_messages(self.dev(), sysex::TEMP_PATCH, true);
                self.send_bulk(&msgs, None)?;
                thread::sleep(TYPE_RELOAD);
                self.send_bulk(&msgs, None)
            }
            Cmd::Param { rel, data } => {
                // Coalesce a burst of knob moves: keep only the latest value per address.
                let mut latest: Vec<(u32, Vec<u8>)> = vec![(rel, data)];
                while let Ok(m) = self.rx.try_recv() {
                    match m {
                        Msg::Cmd(Cmd::Param { rel, data }) => match latest.iter_mut().find(|(a, _)| *a == rel) {
                            Some(e) => e.1 = data,
                            None => latest.push((rel, data)),
                        },
                        Msg::Cmd(other) => self.pending.push_back(other),
                        Msg::Midi(m) => self.on_unsolicited(&m),
                    }
                }
                for (rel, data) in latest {
                    self.dt1(sysex::TEMP_PATCH + rel, &data)?;
                }
                Ok(())
            }
            Cmd::WriteSlot { slot, patch } => {
                if !(1..SLOT_COUNT).contains(&slot) {
                    return Err("Slot PANEL nie jest zapisywalny".into());
                }
                let base = sysex::user_patch(slot);
                let msgs = patch.dt1_messages(self.dev(), base, true);
                self.send_bulk(&msgs, Some(format!("Writing {}", sysex::SLOT_LABELS[slot as usize])))?;
                let back = self.read_patch(base, None)?;
                let verified = back == *patch;
                (self.emit)(Event::Written { slot, verified });
                self.read_names()
            }
            Cmd::ReadSlot(slot) => {
                let p = self.read_patch(sysex::user_patch(slot), Some(format!("Reading {}", sysex::SLOT_LABELS[slot as usize])))?;
                (self.emit)(Event::SlotPatch(slot, Box::new(p)));
                Ok(())
            }
            Cmd::ReadAllSlots => {
                let mut all = vec![];
                for s in 0..SLOT_COUNT {
                    all.push(self.read_patch(sysex::user_patch(s), Some(format!("Backing up {}", sysex::SLOT_LABELS[s as usize])))?);
                }
                (self.emit)(Event::AllSlots(all));
                Ok(())
            }
            Cmd::ReadTemp => {
                let p = self.read_patch(sysex::TEMP_PATCH, None)?;
                (self.emit)(Event::TempPatch(Box::new(p)));
                Ok(())
            }
            Cmd::SelectSlot(s) => {
                self.dt1(sysex::PATCH_SELECT, &[0, s])?;
                (self.emit)(Event::CurrentSlot(s));
                self.pending.push_back(Cmd::ReadTemp);
                Ok(())
            }
            Cmd::Shutdown => Ok(()),
        }
    }

    fn dev(&self) -> u8 {
        self.conn.as_ref().map(|c| c.dev).unwrap_or(0)
    }

    fn raw_send(&mut self, m: &[u8]) -> Result<(), String> {
        let Some(c) = self.conn.as_mut() else { return Err("Amp not connected".into()) };
        if let Err(e) = c.out.send(m) {
            self.conn = None;
            (self.emit)(Event::Disconnected(e.to_string()));
            return Err(format!("Connection lost: {e}"));
        }
        Ok(())
    }

    fn dt1(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        let m = sysex::dt1(self.dev(), addr, data);
        self.raw_send(&m)
    }

    fn send_bulk(&mut self, msgs: &[Vec<u8>], label: Option<String>) -> Result<(), String> {
        for (i, m) in msgs.iter().enumerate() {
            self.raw_send(m)?;
            if let Some(l) = &label {
                (self.emit)(Event::Progress { label: l.clone(), done: i + 1, total: msgs.len() });
            }
            thread::sleep(DT1_GAP);
        }
        Ok(())
    }

    /// Wait for MIDI matching `pred`, queueing commands and dispatching unrelated MIDI meanwhile.
    fn wait_midi<T>(&mut self, timeout: Duration, mut pred: impl FnMut(&[u8]) -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.checked_duration_since(Instant::now())?;
            match self.rx.recv_timeout(left) {
                Ok(Msg::Midi(m)) => {
                    if let Some(t) = pred(&m) {
                        return Some(t);
                    }
                    self.on_unsolicited(&m);
                }
                Ok(Msg::Cmd(c)) => self.pending.push_back(c),
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn rq1(&mut self, addr: u32, size: u32) -> Result<Vec<u8>, String> {
        let m = sysex::rq1(self.dev(), addr, size);
        self.raw_send(&m)?;
        let mut buf = vec![0u8; size as usize];
        let mut got = 0usize;
        while got < size as usize {
            let chunk = self.wait_midi(RQ1_TIMEOUT, |m| {
                let d = sysex::parse_dt1(m)?;
                (d.addr >= addr && d.addr < addr + size).then_some(d)
            });
            let Some(d) = chunk else { return Err(format!("No reply from the amp ({:08X})", addr)) };
            let off = (d.addr - addr) as usize;
            let n = d.data.len().min(buf.len() - off);
            buf[off..off + n].copy_from_slice(&d.data[..n]);
            got += n;
        }
        Ok(buf)
    }

    fn read_patch(&mut self, base: u32, label: Option<String>) -> Result<Patch, String> {
        let mut p = Patch::default();
        let blocks = katana_core::map::PATCH_BLOCKS;
        for (i, b) in blocks.iter().enumerate() {
            let data = self.rq1(base + b.addr, b.len as u32)?;
            p.set_block(i, &data);
            if let Some(l) = &label {
                (self.emit)(Event::Progress { label: l.clone(), done: i + 1, total: blocks.len() });
            }
        }
        Ok(p)
    }

    fn read_names(&mut self) -> Result<(), String> {
        let mut names = vec![];
        for s in 0..SLOT_COUNT {
            let n = self.rq1(sysex::user_patch(s), 16)?;
            names.push(String::from_utf8_lossy(&n).trim_end().to_string());
        }
        (self.emit)(Event::SlotNames(names));
        Ok(())
    }

    /// `quiet`: do not report "not found" (hotplug of an unrelated device).
    fn connect(&mut self, quiet: bool) -> Result<(), String> {
        self.conn = None;
        let mut inp = MidiInput::new("katana-studio").map_err(|e| e.to_string())?;
        inp.ignore(Ignore::None);
        let out = MidiOutput::new("katana-studio").map_err(|e| e.to_string())?;
        let pick = |name: String| name.to_uppercase().contains("KATANA");
        let in_port = inp.ports().into_iter().find(|p| inp.port_name(p).map(pick).unwrap_or(false));
        let out_port = out.ports().into_iter().find(|p| out.port_name(p).map(pick).unwrap_or(false));
        let (Some(ip), Some(op)) = (in_port, out_port) else {
            if !quiet {
                (self.emit)(Event::Disconnected("No Katana found on USB".into()));
            }
            return Ok(());
        };
        let port_name = out.port_name(&op).unwrap_or_default();
        let tx = self.tx.clone();
        let _inp = inp
            .connect(&ip, "katana-studio-in", move |_, m, _| {
                let _ = tx.send(Msg::Midi(m.to_vec()));
            }, ())
            .map_err(|e| e.to_string())?;
        let out = out.connect(&op, "katana-studio-out").map_err(|e| e.to_string())?;
        self.conn = Some(Conn { out, _inp, dev: 0x10 });
        self.raw_send(&sysex::IDENTITY_REQUEST)?;
        let Some(dev) = self.wait_midi(Duration::from_millis(1500), sysex::parse_identity) else {
            self.conn = None;
            (self.emit)(Event::Disconnected("Katana does not answer the Identity Request".into()));
            return Ok(());
        };
        if let Some(c) = self.conn.as_mut() {
            c.dev = dev;
        }
        // Editor mode: the amp reports knob turns and channel changes as DT1.
        self.dt1(sysex::EDITOR_COMMUNICATION_MODE, &[1])?;
        (self.emit)(Event::Connected { port: port_name });
        self.read_names()?;
        let cur = self.rq1(sysex::CURRENT_PATCH_NUMBER, 2)?;
        (self.emit)(Event::CurrentSlot(cur[1]));
        self.pending.push_back(Cmd::ReadTemp);
        Ok(())
    }

    fn on_unsolicited(&mut self, m: &[u8]) {
        let Some(d) = sysex::parse_dt1(m) else { return };
        if d.addr == sysex::CURRENT_PATCH_NUMBER && d.data.len() >= 2 {
            (self.emit)(Event::CurrentSlot(d.data[1]));
            self.pending.push_back(Cmd::ReadTemp);
        } else if d.addr >= sysex::TEMP_PATCH && d.addr < sysex::TEMP_PATCH + sysex::lin(0x0001_0000) {
            (self.emit)(Event::TempEdited { rel: d.addr - sysex::TEMP_PATCH, data: d.data });
        }
    }
}

#[cfg(target_os = "linux")]
/// Hotplug without polling: inotify on /dev/snd wakes this thread only when a `midiC*` node is
/// created or removed; after a short settle delay (ALSA sequencer client setup) the actor
/// re-scans. Silently does nothing if inotify is unavailable.
fn watch_hotplug(tx: Sender<Msg>) {
    let _ = thread::Builder::new().name("katana-hotplug".into()).spawn(move || unsafe {
        let fd = libc::inotify_init1(libc::IN_CLOEXEC);
        if fd < 0 {
            return;
        }
        let dir = b"/dev/snd\0";
        if libc::inotify_add_watch(fd, dir.as_ptr().cast(), libc::IN_CREATE | libc::IN_DELETE) < 0 {
            libc::close(fd);
            return;
        }
        let mut buf = [0u8; 4096];
        loop {
            let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
            if n <= 0 {
                break;
            }
            // walk the event records, react to midi nodes only
            let mut off = 0usize;
            let mut midi = false;
            while off + std::mem::size_of::<libc::inotify_event>() <= n as usize {
                let ev = &*(buf.as_ptr().add(off) as *const libc::inotify_event);
                let name_at = off + std::mem::size_of::<libc::inotify_event>();
                let name = &buf[name_at..name_at + ev.len as usize];
                midi |= name.starts_with(b"midiC");
                off = name_at + ev.len as usize;
            }
            if midi {
                thread::sleep(Duration::from_millis(700));
                if tx.send(Msg::Cmd(Cmd::Hotplug)).is_err() {
                    break;
                }
            }
        }
        libc::close(fd);
    });
}

/// Hotplug without polling on Windows: Configuration Manager calls back on its own thread
/// pool when a USB device interface arrives or leaves (no window, no message loop). One plug
/// fires several interfaces, and WinMM lists the MIDI port only once the driver has loaded,
/// so events are coalesced and the actor re-scans 1.5 s after the last one.
#[cfg(windows)]
fn watch_hotplug(tx: Sender<Msg>) {
    use std::ffi::c_void;
    use windows_sys::core::GUID;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
    // GUID_DEVINTERFACE_USB_DEVICE: any USB device, whichever driver (BOSS or class) the amp uses
    const USB_DEVICE: GUID = GUID::from_u128(0xA5DCBF10_6530_11D2_901F_00C04FB951ED);

    unsafe extern "system" fn on_change(
        _h: HCMNOTIFICATION,
        ctx: *const c_void,
        action: CM_NOTIFY_ACTION,
        _data: *const CM_NOTIFY_EVENT_DATA,
        _size: u32,
    ) -> u32 {
        if action == CM_NOTIFY_ACTION_DEVICEINTERFACEARRIVAL || action == CM_NOTIFY_ACTION_DEVICEINTERFACEREMOVAL {
            let _ = (*(ctx as *const Sender<()>)).send(());
        }
        0
    }

    let (ev_tx, ev_rx) = channel::<()>();
    let ctx = Box::into_raw(Box::new(ev_tx));
    let ok = unsafe {
        let mut filter: CM_NOTIFY_FILTER = std::mem::zeroed();
        filter.cbSize = std::mem::size_of::<CM_NOTIFY_FILTER>() as u32;
        filter.FilterType = CM_NOTIFY_FILTER_TYPE_DEVICEINTERFACE;
        filter.u.DeviceInterface.ClassGuid = USB_DEVICE;
        let mut h: HCMNOTIFICATION = std::mem::zeroed();
        CM_Register_Notification(&filter, ctx as *const c_void, Some(on_change), &mut h) == CR_SUCCESS
    };
    if !ok {
        drop(unsafe { Box::from_raw(ctx) });
        return;
    }
    // the registration (and `ctx`) live for the rest of the process
    let _ = thread::Builder::new().name("katana-hotplug".into()).spawn(move || {
        while ev_rx.recv().is_ok() {
            while ev_rx.recv_timeout(Duration::from_millis(1500)).is_ok() {}
            if tx.send(Msg::Cmd(Cmd::Hotplug)).is_err() {
                break;
            }
        }
    });
}

#[cfg(not(any(target_os = "linux", windows)))]
fn watch_hotplug(_tx: Sender<Msg>) {}
