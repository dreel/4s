//! The song (RFC 0008 phase B): each track's arrangement of clip placements
//! on a timeline in ticks, and where song mode plays from. Song mode and its
//! loop are parameters (`song.mode`, `song.loop`, `song.loop_start`,
//! `song.loop_end`), read by the engine directly.

use super::*;

impl Core {
    /// The song's end: where its last placement ends.
    pub(super) fn song_length(&self) -> u32 {
        self.tracks.values().filter_map(|t| t.placements.last()).map(Placement::end).max().unwrap_or(0)
    }

    pub(super) fn song_info(&self) -> SongInfo {
        SongInfo {
            length: self.song_length(),
            tracks: self.instruments.iter().map(|i| self.track_info(&i.id)).collect(),
        }
    }

    /// Whether song mode is on.
    pub(super) fn song_mode(&self) -> bool {
        self.global(params::SONG_MODE) >= 0.5
    }

    /// The span song mode loops, as the engine sees it.
    pub(super) fn song_loop(&self) -> Option<(u32, u32)> {
        match self.global(params::LOOP).round() as i32 {
            1 => Some((0, self.song_length())).filter(|(_, e)| *e > 0),
            2 => {
                let (s, e) = (self.global(params::LOOP_START) as u32, self.global(params::LOOP_END) as u32);
                (e > s).then_some((s * TICKS_PER_BAR, e * TICKS_PER_BAR))
            }
            _ => None,
        }
    }

    /// Replace a track's arrangement: checked (in range, clips exist, not
    /// overlapping, not too many); the engine gets only the difference.
    pub(super) fn set_arrangement(&mut self, id: &str, mut placements: Vec<Placement>) -> Result<(), RpcError> {
        placements.sort_by_key(|p| p.start);
        // A placement loops its clip, so an offset past the clip's length
        // (the right part of one that was cut) is the same as its remainder.
        for p in &mut placements {
            if self.track(id).clips.contains_key(&p.clip) {
                p.offset %= self.clip_len(id, p.clip).max(1);
            }
        }
        let t = self.track(id);
        if placements.len() > MAX_PLACEMENTS {
            return Err(RpcError::failed(format!("at most {MAX_PLACEMENTS} placements on a track")));
        }
        for (i, p) in placements.iter().enumerate() {
            if !t.clips.contains_key(&p.clip) {
                return Err(RpcError::invalid(format!("no clip {} on '{id}'", p.clip)));
            }
            if p.length == 0 || p.end() > MAX_SONG_TICKS || p.start >= MAX_SONG_TICKS {
                return Err(RpcError::invalid(format!(
                    "placements are 1.. ticks long and end by tick {MAX_SONG_TICKS} (bar 999)"
                )));
            }
            if p.offset >= MAX_CLIP_TICKS {
                return Err(RpcError::invalid(format!("offset must be under {MAX_CLIP_TICKS} ticks")));
            }
            if placements.get(i + 1).is_some_and(|q| q.start < p.end()) {
                return Err(RpcError::invalid("placements on a track must not overlap"));
            }
        }
        let old = t.placements.clone();
        let removed: Vec<u32> = old.iter().filter(|p| !placements.contains(p)).map(|p| p.start).collect();
        let added: Vec<Placement> = placements.iter().filter(|p| !old.contains(p)).copied().collect();
        if removed.is_empty() && added.is_empty() {
            return Ok(());
        }
        self.ensure_room(removed.len() + added.len())?;
        let slot = self.slot(id)?;
        for start in removed {
            self.send(Command::RemovePlacement { slot, start });
        }
        for p in &added {
            let clip = self.track(id).clips[&p.clip].engine;
            let placement = fours_engine::Placement { start: p.start, length: p.length, offset: p.offset, clip };
            self.send(Command::AddPlacement { slot, placement });
        }
        self.tracks.get_mut(id).expect("checked").placements = placements;
        Ok(())
    }

    /// Song RPCs and `transport.locate`. Any other request is handed back.
    pub(super) fn handle_song(&mut self, req: Request, origin: &str, client: &str) -> Result<RpcResult, Request> {
        Ok(match req {
            Request::SongGet(_) => ok(self.song_info()),
            Request::SongPlace(p) => (|| {
                let id = self.clip_target(p.instrument.as_deref(), client)?;
                let n = self.clip_id(&id, p.clip)?;
                let length = p.length.unwrap_or_else(|| self.clip_len(&id, n));
                let placement = Placement { clip: n, start: p.start, length, offset: p.offset.unwrap_or(0) };
                if length == 0 || p.start.checked_add(length).is_none_or(|e| e > MAX_SONG_TICKS) {
                    return Err(RpcError::invalid(format!("a placement is 1.. ticks long and ends by tick {MAX_SONG_TICKS}")));
                }
                let placements = place(&self.track(&id).placements, placement);
                self.set_arrangement(&id, placements)?;
                self.track_changed(&id, origin);
                Ok(self.track_info(&id))
            })()
            .and_then(ok),
            Request::SongRemove(p) => (|| {
                let id = self.clip_target(p.instrument.as_deref(), client)?;
                let mut placements = self.track(&id).placements.clone();
                let before = placements.len();
                placements.retain(|q| q.start != p.start);
                if placements.len() == before {
                    return Err(RpcError::invalid(format!("no placement on '{id}' starts at tick {}", p.start)));
                }
                self.set_arrangement(&id, placements)?;
                self.track_changed(&id, origin);
                Ok(self.track_info(&id))
            })()
            .and_then(ok),
            Request::SongMove(p) => (|| {
                let id = self.clip_target(p.instrument.as_deref(), client)?;
                let placements = self.track(&id).placements.clone();
                let Some(q) = placements.iter().find(|q| q.start == p.start).copied() else {
                    return Err(RpcError::invalid(format!("no placement on '{id}' starts at tick {}", p.start)));
                };
                if p.to.checked_add(q.length).is_none_or(|e| e > MAX_SONG_TICKS) {
                    return Err(RpcError::invalid(format!("it would end past tick {MAX_SONG_TICKS}")));
                }
                // Lifted off, then laid down at its new start (cutting what
                // it lands on).
                let rest: Vec<Placement> = placements.into_iter().filter(|x| x.start != p.start).collect();
                self.set_arrangement(&id, place(&rest, Placement { start: p.to, ..q }))?;
                self.track_changed(&id, origin);
                Ok(self.track_info(&id))
            })()
            .and_then(ok),
            Request::TransportLocate(p) => {
                if p.tick >= MAX_SONG_TICKS {
                    return Ok(Err(RpcError::invalid(format!("locate to a tick under {MAX_SONG_TICKS}"))));
                }
                self.send(Command::Locate { tick: p.tick });
                self.locate = p.tick;
                self.emit(origin, Event::Located { tick: p.tick });
                ok(self.transport_state())
            }
            other => return Err(other),
        })
    }
}
