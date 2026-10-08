//! Seats, MIDI input routing, held notes, and knob pickup (RFC 0006).
//!
//! A seat is one performer's setup (focus, knob page, note bindings, CC
//! maps), saved in the project. Clients sit in seats; the engine host's own
//! devices use the host seat. MIDI input from a logical device is resolved
//! here, on the control side, into note-ons/offs and parameter changes.

use super::*;
use fours_protocol::slug;

/// Velocity of a knob within this distance of the current value takes over
/// at once (pickup).
const PICKUP_SNAP: f64 = 0.02;

pub(super) struct SeatState {
    pub config: SeatConfig,
    pub saved: bool,
    pub learning: Option<String>,
}

pub(super) struct ClientState {
    /// `name#n`, as shown in occupants.
    pub label: String,
    pub user: Option<String>,
    pub seat: Option<String>,
    /// Connected from the engine host itself.
    pub local: bool,
    /// Re-match by user when the seat goes away (project load).
    pub auto: bool,
    /// When it took its seat (the latest local one decides the host seat).
    pub seated_at: u64,
}

/// A note held by someone: `key` is the input note, `note` what it played
/// (after transpose) on instrument `slot`.
pub(super) struct Held {
    pub holder: String,
    pub key: u8,
    pub slot: u8,
    pub note: u8,
}

/// Pickup state of one knob: the parameter it drives, the value it last
/// set (normalized), and its last position.
pub(super) struct Pickup {
    pub path: String,
    pub sent: Option<f64>,
    pub last_in: Option<f64>,
}

/// Seat and note checks shared by RPC edits and project loading.
pub(super) fn check_seat_config(c: &SeatConfig) -> Result<(), RpcError> {
    let channel = |ch: Option<u8>| match ch {
        Some(c) if !(1..=16).contains(&c) => Err(RpcError::invalid("MIDI channel must be 1..16")),
        _ => Ok(()),
    };
    for b in &c.bindings {
        validate_name("device", &b.device).map_err(RpcError::invalid)?;
        channel(b.channel)?;
        if b.low.is_some_and(|n| n > 127) || b.high.is_some_and(|n| n > 127) {
            return Err(RpcError::invalid("notes must be 0..127"));
        }
        if b.target != "focus" {
            validate_instrument_id(&b.target).map_err(RpcError::invalid)?;
        }
    }
    for m in &c.cc {
        validate_name("device", &m.device).map_err(RpcError::invalid)?;
        channel(m.channel)?;
        if m.cc > 127 {
            return Err(RpcError::invalid("CC numbers are 0..127"));
        }
    }
    for k in &c.knobs {
        validate_name("device", &k.device).map_err(RpcError::invalid)?;
        channel(k.channel)?;
        if k.ccs.len() > crate::controller::GRID || k.ccs.iter().any(|c| *c > 127) {
            return Err(RpcError::invalid("at most 8 knob CCs, each 0..127"));
        }
    }
    Ok(())
}

impl Core {
    // ---- seat queries ------------------------------------------------------

    pub(super) fn seats_state(&self) -> SeatsState {
        let seats = self
            .seats
            .iter()
            .map(|(name, s)| {
                let mut occupants: Vec<String> = self
                    .clients
                    .values()
                    .filter(|c| c.seat.as_deref() == Some(name.as_str()))
                    .map(|c| c.label.clone())
                    .collect();
                occupants.sort();
                Seat {
                    name: name.clone(),
                    saved: s.saved,
                    config: s.config.clone(),
                    occupants,
                    learning: s.learning.clone(),
                }
            })
            .collect();
        SeatsState { seats, host: self.host_seat.clone() }
    }

    fn seat_list(&self, client: &str) -> SeatListResult {
        let s = self.seats_state();
        SeatListResult { seats: s.seats, you: self.client_seat(client), host: s.host }
    }

    fn seat_info(&self, name: &str) -> Seat {
        self.seats_state().seats.into_iter().find(|s| s.name == name).expect("seat exists")
    }

    pub(super) fn client_seat(&self, client: &str) -> Option<String> {
        self.clients.get(client).and_then(|c| c.seat.clone())
    }

    /// The seat an edit applies to: the one named, else the caller's.
    fn seat_for(&self, seat: Option<&str>, client: &str) -> Result<String, RpcError> {
        match seat {
            Some(s) => {
                self.check_seat(s)?;
                Ok(s.to_string())
            }
            None => self.client_seat(client).ok_or_else(|| {
                RpcError::invalid("you have no seat: pass `seat`, or join one (`4s seat claim` / `4s seat create`)")
            }),
        }
    }

    pub(super) fn check_seat(&self, name: &str) -> Result<(), RpcError> {
        if self.seats.contains_key(name) {
            return Ok(());
        }
        let names: Vec<&str> = self.seats.keys().map(String::as_str).collect();
        Err(RpcError::invalid(format!("no seat '{name}' (seats: {})", names.join(", "))))
    }

    /// A seat's focused instrument: its choice if that instrument exists,
    /// else the first instrument.
    pub(super) fn seat_focus(&self, seat: &str) -> Option<String> {
        let chosen = self.seats.get(seat).and_then(|s| s.config.focus.clone());
        chosen
            .filter(|f| self.instruments.iter().any(|i| &i.id == f))
            .or_else(|| self.instruments.first().map(|i| i.id.clone()))
    }

    /// The focused instrument's pages and the one in use.
    pub(super) fn seat_page(&self, seat: &str) -> (Vec<KnobPage>, Option<KnobPage>) {
        let Some(focus) = self.seat_focus(seat) else { return (Vec::new(), None) };
        let Ok(inst) = self.find_instrument(&focus) else { return (Vec::new(), None) };
        let pages = instrument::knob_pages(inst.kind, &inst.id);
        let chosen = self.seats.get(seat).and_then(|s| s.config.knob_page.clone());
        let page = chosen
            .and_then(|id| pages.iter().find(|p| p.id == id).cloned())
            .or_else(|| pages.first().cloned());
        (pages, page)
    }

    /// Default instrument for a client's notes: its seat's focus, else the
    /// host seat's.
    pub(super) fn default_instrument(&self, client: &str) -> Result<String, RpcError> {
        let seat = self.client_seat(client).unwrap_or_else(|| self.host_seat.clone());
        self.seat_focus(&seat).ok_or_else(|| RpcError::invalid("no instruments"))
    }

    /// The one seat matching a user name (exactly, ignoring case, or as a
    /// slug), if exactly one does.
    fn match_user(&self, user: &str) -> Option<String> {
        let s = slug(user);
        let m: Vec<&String> =
            self.seats.keys().filter(|n| n.eq_ignore_ascii_case(user) || **n == s).collect();
        (m.len() == 1).then(|| m[0].clone())
    }

    fn free_seat_name(&self, base: &str) -> String {
        if !self.seats.contains_key(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}{n}")).find(|n| !self.seats.contains_key(n)).unwrap()
    }

    fn new_seat(&mut self, name: &str, saved: bool) {
        self.seats.insert(name.to_string(), SeatState { config: SeatConfig::default(), saved, learning: None });
    }

    // ---- seat bookkeeping ----------------------------------------------------

    /// Decide which seat the host's own devices use:
    /// 1. the seat pinned in `midi-devices.json` (created if missing);
    /// 2. else the seat of the latest local client to take one;
    /// 3. else the seat matching the host's user;
    /// 4. else, in a project without seats, a new seat for the host's user;
    /// 5. else a session-only `local` seat (devices play its focus).
    ///
    /// Then drop session-only seats nobody sits in.
    pub(super) fn refresh_host_seat(&mut self) {
        let local = self
            .clients
            .values()
            .filter(|c| c.local && c.seat.is_some())
            .max_by_key(|c| c.seated_at)
            .and_then(|c| c.seat.clone());
        let name = if let Some(p) = self.hardware.pinned_seat().map(str::to_string) {
            if !self.seats.contains_key(&p) {
                self.new_seat(&p, true);
            }
            p
        } else if let Some(s) = local {
            s
        } else if let Some(s) = self.match_user(&self.host_user.clone()) {
            s
        } else if self.seats.values().all(|s| !s.saved) {
            let n = slug(&self.host_user);
            if !self.seats.contains_key(&n) {
                self.new_seat(&n, true);
            }
            n
        } else {
            if !self.seats.contains_key("local") {
                self.new_seat("local", false);
            }
            "local".to_string()
        };
        self.host_seat = name;
        let occupied: Vec<String> = self.clients.values().filter_map(|c| c.seat.clone()).collect();
        let host = self.host_seat.clone();
        self.seats.retain(|n, s| s.saved || *n == host || occupied.contains(n));
    }

    /// Seats or occupancy changed: settle the host seat, tell everyone, and
    /// mark the project modified if saved seat contents changed.
    pub(super) fn seats_changed(&mut self, origin: &str, dirty: bool) {
        self.refresh_host_seat();
        let state = self.seats_state();
        self.emit(origin, Event::Seats { state });
        if dirty {
            self.mark_dirty(origin);
        }
        self.refresh_controller(origin, true);
    }

    fn seat_edited(&mut self, seat: &str, origin: &str) -> Seat {
        let saved = self.seats.get(seat).is_some_and(|s| s.saved);
        self.seats_changed(origin, saved);
        self.seat_info(seat)
    }

    fn sit(&mut self, client: &str, seat: Option<String>) {
        self.seat_clock += 1;
        let at = self.seat_clock;
        if let Some(c) = self.clients.get_mut(client) {
            c.seat = seat;
            c.seated_at = at;
        }
    }

    /// A client said hello. Seats it: in `seat` if given, else (with
    /// `auto`) in the one seat matching its user, else in a new seat for it
    /// when the project has none. Returns its seat and whether the user
    /// should be asked to choose.
    #[allow(clippy::too_many_arguments)]
    pub fn client_hello(
        &mut self,
        client: &str,
        label: &str,
        user: Option<String>,
        seat: Option<String>,
        auto: bool,
        local: bool,
        origin: &str,
    ) -> Result<(Option<String>, bool), RpcError> {
        if let Some(s) = &seat {
            self.check_seat(s)?;
        }
        let user = user.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
        self.clients.insert(
            client.to_string(),
            ClientState { label: label.to_string(), user: user.clone(), seat: None, local, auto, seated_at: 0 },
        );
        let chosen = seat.or_else(|| if auto { self.auto_seat(user.as_deref()) } else { None });
        self.sit(client, chosen.clone());
        self.seats_changed(origin, false);
        Ok((chosen.clone(), auto && chosen.is_none()))
    }

    /// The seat a user joins without asking, creating one in a project
    /// that has no saved seats (only the host's untouched default).
    fn auto_seat(&mut self, user: Option<&str>) -> Option<String> {
        let user = user?;
        if let Some(s) = self.match_user(user) {
            return Some(s);
        }
        let only_default = self.seats.iter().all(|(n, s)| !s.saved || (*n == self.host_seat && s.config == SeatConfig::default()));
        if only_default && self.clients.values().all(|c| c.seat.is_none()) {
            let n = self.free_seat_name(&slug(user));
            self.new_seat(&n, true);
            return Some(n);
        }
        None
    }

    pub fn client_gone(&mut self, client: &str) {
        self.release_held_by(client);
        if self.clients.remove(client).is_some() {
            self.seats_changed("engine", false);
        }
    }

    /// After a project load: clients whose seat is gone are seated again as
    /// at hello (matched by user, or given a new seat in a project without
    /// seats), local clients first so the host keeps its user's seat.
    pub(super) fn reseat_clients(&mut self) {
        let mut lost: Vec<(String, Option<String>, bool, bool)> = self
            .clients
            .iter()
            .filter(|(_, c)| c.seat.as_ref().is_some_and(|s| !self.seats.contains_key(s)))
            .map(|(k, c)| (k.clone(), c.user.clone(), c.auto, c.local))
            .collect();
        for (client, ..) in &lost {
            self.sit(client, None);
        }
        lost.sort_by_key(|(k, _, _, local)| (!local, k.clone()));
        for (client, user, auto, _) in lost {
            let seat = if auto { self.auto_seat(user.as_deref()) } else { None };
            self.sit(&client, seat);
        }
    }

    // ---- seat RPCs -----------------------------------------------------------

    fn seat_claim(&mut self, client: &str, name: &str, origin: &str) -> Result<SeatListResult, RpcError> {
        self.check_seat(name)?;
        if !self.clients.contains_key(client) {
            return Err(RpcError::invalid("say session.hello first"));
        }
        self.sit(client, Some(name.to_string()));
        self.seats_changed(origin, false);
        Ok(self.seat_list(client))
    }

    fn seat_create(&mut self, client: &str, p: SeatCreateParams, origin: &str) -> Result<SeatListResult, RpcError> {
        let Some(c) = self.clients.get(client) else { return Err(RpcError::invalid("say session.hello first")) };
        let name = match p.name {
            Some(n) => {
                validate_name("seat", &n).map_err(RpcError::invalid)?;
                if self.seats.contains_key(&n) {
                    return Err(RpcError::invalid(format!("a seat '{n}' already exists; join it with seat.claim")));
                }
                n
            }
            None => self.free_seat_name(&slug(c.user.as_deref().unwrap_or("seat"))),
        };
        let saved = p.saved.unwrap_or(true);
        self.new_seat(&name, saved);
        self.sit(client, Some(name));
        self.seats_changed(origin, saved);
        Ok(self.seat_list(client))
    }

    fn seat_leave(&mut self, client: &str, origin: &str) -> SeatListResult {
        self.sit(client, None);
        self.seats_changed(origin, false);
        self.seat_list(client)
    }

    fn seat_remove(&mut self, client: &str, name: &str, origin: &str) -> Result<SeatListResult, RpcError> {
        self.check_seat(name)?;
        let saved = self.seats.remove(name).is_some_and(|s| s.saved);
        let gone: Vec<String> =
            self.clients.iter().filter(|(_, c)| c.seat.as_deref() == Some(name)).map(|(k, _)| k.clone()).collect();
        for c in gone {
            self.sit(&c, None);
        }
        if self.hardware.pinned_seat() == Some(name) {
            self.hardware.set_seat(None);
        }
        self.pickups.retain(|k, _| !k.starts_with(&format!("{name}/")));
        self.seats_changed(origin, saved);
        Ok(self.seat_list(client))
    }

    fn edit_seat(
        &mut self,
        seat: Option<&str>,
        client: &str,
        origin: &str,
        f: impl FnOnce(&mut SeatState) -> Result<(), RpcError>,
    ) -> Result<Seat, RpcError> {
        let name = self.seat_for(seat, client)?;
        let s = self.seats.get_mut(&name).expect("checked");
        let mut edited = SeatState { config: s.config.clone(), saved: s.saved, learning: s.learning.clone() };
        f(&mut edited)?;
        check_seat_config(&edited.config)?;
        *self.seats.get_mut(&name).expect("checked") = edited;
        Ok(self.seat_edited(&name, origin))
    }

    /// Seat RPCs. `None` if `req` is not one of them.
    pub(super) fn handle_seat(&mut self, req: &Request, origin: &str, client: &str) -> Option<RpcResult> {
        Some(match req.clone() {
            Request::SeatList(_) => ok(self.seat_list(client)),
            Request::SeatClaim(p) => self.seat_claim(client, &p.name, origin).and_then(ok),
            Request::SeatCreate(p) => self.seat_create(client, p, origin).and_then(ok),
            Request::SeatLeave(_) => ok(self.seat_leave(client, origin)),
            Request::SeatRemove(p) => self.seat_remove(client, &p.name, origin).and_then(ok),
            Request::SeatFocus(p) => {
                if let Err(e) = self.find_instrument(&p.instrument) {
                    return Some(Err(e));
                }
                self.edit_seat(p.seat.as_deref(), client, origin, |s| {
                    s.config.focus = Some(p.instrument);
                    Ok(())
                })
                .and_then(ok)
            }
            Request::SeatPage(p) => {
                let name = match self.seat_for(p.seat.as_deref(), client) {
                    Ok(n) => n,
                    Err(e) => return Some(Err(e)),
                };
                let (pages, _) = self.seat_page(&name);
                if !pages.iter().any(|x| x.id == p.page) {
                    let ids: Vec<&str> = pages.iter().map(|x| x.id.as_str()).collect();
                    return Some(Err(RpcError::invalid(format!(
                        "no knob page '{}' on the focused instrument (pages: {})",
                        p.page,
                        ids.join(", ")
                    ))));
                }
                self.edit_seat(Some(&name), client, origin, |s| {
                    s.config.knob_page = Some(p.page);
                    Ok(())
                })
                .and_then(ok)
            }
            Request::SeatBind(p) => self
                .edit_seat(p.seat.as_deref(), client, origin, |s| {
                    s.config.bindings.push(p.binding);
                    Ok(())
                })
                .and_then(ok),
            Request::SeatUnbind(p) => self
                .edit_seat(p.seat.as_deref(), client, origin, |s| {
                    if p.index as usize >= s.config.bindings.len() {
                        return Err(RpcError::invalid(format!(
                            "no binding {} (the seat has {})",
                            p.index,
                            s.config.bindings.len()
                        )));
                    }
                    s.config.bindings.remove(p.index as usize);
                    Ok(())
                })
                .and_then(ok),
            Request::SeatMapCc(p) => {
                if let Err(e) = self.param_id(&p.map.param) {
                    return Some(Err(e));
                }
                self.edit_seat(p.seat.as_deref(), client, origin, |s| {
                    let m = p.map;
                    s.config.cc.retain(|x| !(x.device == m.device && x.cc == m.cc && x.channel == m.channel));
                    s.config.cc.push(m);
                    Ok(())
                })
                .and_then(ok)
            }
            Request::SeatUnmapCc(p) => self
                .edit_seat(p.seat.as_deref(), client, origin, |s| {
                    s.config.cc.retain(|x| {
                        !(x.device == p.device && x.cc == p.cc && p.channel.is_none_or(|c| x.channel == Some(c)))
                    });
                    Ok(())
                })
                .and_then(ok),
            Request::SeatLearnCc(p) => {
                if let Some(path) = &p.param
                    && let Err(e) = self.param_id(path)
                {
                    return Some(Err(e));
                }
                self.edit_seat(p.seat.as_deref(), client, origin, |s| {
                    s.learning = p.param;
                    Ok(())
                })
                .and_then(ok)
            }
            Request::SeatFollowKnobs(p) => self
                .edit_seat(p.seat.as_deref(), client, origin, |s| {
                    let f = p.follow;
                    s.config.knobs.retain(|k| k.device != f.device);
                    if !f.ccs.is_empty() {
                        s.config.knobs.push(f);
                    }
                    Ok(())
                })
                .and_then(ok),
            _ => return None,
        })
    }

    // ---- held notes ----------------------------------------------------------

    /// Start a held note for `holder`. `key` is what the holder will
    /// release it by (the input note).
    pub(super) fn hold_note(&mut self, holder: &str, key: u8, slot: u8, note: u8, velocity: f32) -> Result<(), RpcError> {
        self.ensure_room(1)?;
        self.held.retain(|h| !(h.holder == holder && h.key == key && h.slot == slot));
        self.held.push(Held { holder: holder.to_string(), key, slot, note });
        self.send(Command::NoteOn { slot, note, velocity: velocity.clamp(0.0, 1.0), gate: false });
        Ok(())
    }

    /// Release what `holder` holds by `key` (on `slot` only, if given).
    /// The engine hears a note-off only when nobody else holds that note.
    pub(super) fn release_note(&mut self, holder: &str, key: u8, slot: Option<u8>) {
        let mut released = Vec::new();
        self.held.retain(|h| {
            let hit = h.holder == holder && h.key == key && slot.is_none_or(|s| s == h.slot);
            if hit {
                released.push((h.slot, h.note));
            }
            !hit
        });
        self.send_note_offs(released);
    }

    fn send_note_offs(&mut self, released: Vec<(u8, u8)>) {
        for (slot, note) in released {
            if !self.held.iter().any(|h| h.slot == slot && h.note == note) {
                self.send(Command::NoteOff { slot, note });
            }
        }
    }

    /// Release every note held by `holder`: a client connection (with the
    /// devices it fed through `midi.input`), or `midi:<device>`. Sent even
    /// when the queue looks full: there is no caller to report an error to,
    /// and a dropped note-off is logged by `send`.
    pub fn release_held_by(&mut self, holder: &str) {
        let inputs = format!("input:{holder}:");
        let mut released = Vec::new();
        self.held.retain(|h| {
            let hit = h.holder == holder || h.holder.starts_with(&inputs);
            if hit {
                released.push((h.slot, h.note));
            }
            !hit
        });
        self.send_note_offs(released);
    }

    /// Drop held notes whose holder matches `gone`, e.g. a device that was
    /// unplugged.
    pub(super) fn release_held_where(&mut self, gone: impl Fn(&str) -> bool) {
        let mut released = Vec::new();
        self.held.retain(|h| {
            let hit = gone(&h.holder);
            if hit {
                released.push((h.slot, h.note));
            }
            !hit
        });
        self.send_note_offs(released);
    }

    // ---- input routing -------------------------------------------------------

    /// One raw MIDI message from logical `device` in `seat`. Notes it
    /// starts are held by `holder`.
    pub(super) fn device_input(
        &mut self,
        seat: &str,
        device: &str,
        profile: DeviceProfile,
        holder: &str,
        d: &[u8],
        origin: &str,
    ) {
        if profile == DeviceProfile::LividBlock {
            match decode_block(&self.block_map, d) {
                Some(BlockInput::Pad { row, col, pressed }) => {
                    let _ = self.controller_pad(seat, row as u32, col as u32, pressed, origin);
                }
                Some(BlockInput::Knob { index, value }) => {
                    let key = format!("{seat}/{device}/k{index}");
                    let _ = self.page_knob(seat, index, value, Some(key), origin);
                }
                None => {}
            }
            return;
        }
        if d.len() < 3 {
            return;
        }
        let (status, channel, a, b) = (d[0] & 0xf0, (d[0] & 0x0f) + 1, d[1], d[2]);
        match status {
            0x90 if b > 0 => self.input_note_on(seat, device, holder, channel, a, b as f32 / 127.0),
            0x80 | 0x90 => self.release_note(holder, a, None),
            0xb0 => self.input_cc(seat, device, channel, a, b as f64 / 127.0, origin),
            _ => {}
        }
    }

    fn input_note_on(&mut self, seat: &str, device: &str, holder: &str, channel: u8, note: u8, velocity: f32) {
        let Some(s) = self.seats.get(seat) else { return };
        let bound: Vec<NoteBinding> = s.config.bindings.iter().filter(|b| b.device == device).cloned().collect();
        // A device without bindings in this seat plays the seat's focus.
        let bindings = if bound.is_empty() {
            vec![NoteBinding { device: device.into(), channel: None, low: None, high: None, transpose: 0, target: "focus".into() }]
        } else {
            bound
        };
        for b in bindings.iter().filter(|b| b.matches(device, channel, note)) {
            let target = if b.target == "focus" { self.seat_focus(seat) } else { Some(b.target.clone()) };
            let (Some(slot), Ok(out)) = (target.and_then(|t| self.slot_of(&t)), u8::try_from(note as i16 + b.transpose as i16))
            else {
                continue;
            };
            if out <= 127 {
                let _ = self.hold_note(holder, note, slot, out, velocity);
            }
        }
    }

    fn input_cc(&mut self, seat: &str, device: &str, channel: u8, cc: u8, value: f64, origin: &str) {
        let Some(s) = self.seats.get_mut(seat) else { return };
        if let Some(param) = s.learning.take() {
            let map = CcMap { device: device.into(), channel: Some(channel), cc, param, pickup: true };
            s.config.cc.retain(|x| !(x.device == map.device && x.cc == map.cc && x.channel == map.channel));
            s.config.cc.push(map);
            let saved = s.saved;
            self.seats_changed(origin, saved);
            return;
        }
        let maps: Vec<CcMap> = s
            .config
            .cc
            .iter()
            .filter(|m| m.device == device && m.cc == cc && m.channel.is_none_or(|c| c == channel))
            .cloned()
            .collect();
        let knobs: Vec<(usize, bool)> = s
            .config
            .knobs
            .iter()
            .filter(|k| k.device == device && k.channel.is_none_or(|c| c == channel))
            .filter_map(|k| k.ccs.iter().position(|c| *c == cc).map(|i| (i, k.pickup)))
            .collect();
        for m in maps {
            let key = m.pickup.then(|| format!("{seat}/{device}/cc{channel}.{cc}"));
            let _ = self.knob_to(&m.param, value, key, origin);
        }
        for (index, pickup) in knobs {
            let key = pickup.then(|| format!("{seat}/{device}/k{index}"));
            let _ = self.page_knob(seat, index, value, key, origin);
        }
    }

    /// Knob `index` of the seat's current page.
    pub(super) fn page_knob(
        &mut self,
        seat: &str,
        index: usize,
        value: f64,
        pickup: Option<String>,
        origin: &str,
    ) -> Result<(), RpcError> {
        let (_, page) = self.seat_page(seat);
        let Some(path) = page.and_then(|p| p.params.get(index).cloned()) else { return Ok(()) };
        self.knob_to(&path, value, pickup, origin)
    }

    /// Set a parameter from a knob position (0..1 over its range). With a
    /// pickup key, the knob only takes over once it reaches the current
    /// value: it is close, it passed it, or nothing else changed the
    /// parameter since this knob last set it.
    pub(super) fn knob_to(&mut self, path: &str, value: f64, pickup: Option<String>, origin: &str) -> Result<(), RpcError> {
        let id = self.param_id(path)?;
        let v = value.clamp(0.0, 1.0);
        let (min, max) = match self.params[id].info.kind {
            ParamKind::Continuous { min, max } => (min, max),
            ParamKind::Integer { min, max } => (min as f64, max as f64),
            ParamKind::Toggle => (0.0, 1.0),
        };
        let norm = |x: f64| if max > min { (x - min) / (max - min) } else { 0.0 };
        if let Some(key) = pickup {
            let current = norm(self.params[id].value);
            let p = self.pickups.entry(key.clone()).or_insert(Pickup { path: path.into(), sent: None, last_in: None });
            if p.path != path {
                *p = Pickup { path: path.into(), sent: None, last_in: None };
            }
            let ours = p.sent.is_some_and(|s| (s - current).abs() < 1e-9);
            let near = (v - current).abs() <= PICKUP_SNAP;
            let crossed = p.last_in.is_some_and(|l| (l - current) * (v - current) <= 0.0);
            p.last_in = Some(v);
            if !(ours || near || crossed) {
                return Ok(());
            }
            let set = self.set_param(path, min + v * (max - min), origin)?;
            if let Some(p) = self.pickups.get_mut(&key) {
                p.sent = Some(norm(set.value));
            }
            return Ok(());
        }
        self.set_param(path, min + v * (max - min), origin)?;
        Ok(())
    }
}
