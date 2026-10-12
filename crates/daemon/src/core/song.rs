//! The song (RFC 0008 phase B, RFC 0009): each track's arrangement of clip
//! placements on a timeline in ticks, where play starts from, and clip
//! launching. The transport always plays the arrangement; its loop is
//! parameters (`song.loop`, `song.loop_start`, `song.loop_end`), read by the
//! engine directly. A track can loop a launched clip instead (`clip.launch`)
//! until it goes back (`song.back`); while playing, both land on the next
//! `transport.launch_quantize` boundary, which the engine reports
//! (`Feedback::Launched`).

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

    /// Launch a clip on a track (or, `None`, go back to its arrangement):
    /// at once while stopped, else at the next quantize boundary (`now`: the
    /// next tick).
    pub(super) fn launch(&mut self, id: &str, clip: Option<u32>, now: bool) -> Result<(), RpcError> {
        self.ensure_room(1)?;
        let slot = self.slot(id)?;
        let engine = clip.map(|n| self.track(id).clips[&n].engine);
        self.send(Command::Launch { slot, clip: engine, now });
        let playing = self.playing;
        let t = self.tracks.get_mut(id).expect("checked");
        t.launched = clip;
        if !playing {
            t.playing = clip.map(|n| (n, 0));
        }
        Ok(())
    }

    /// A track's launch state changed: it is saved, but not undone.
    pub(super) fn launch_changed(&mut self, id: &str, origin: &str) {
        self.track_changed(id, origin);
        self.refresh_controller(origin, false);
    }

    /// The engine switched a slot to a launched clip (or back) at `tick`
    /// from play, song position `pos`. A take into it writes what was
    /// played into the old target and goes on into the new one.
    pub(super) fn launched(&mut self, slot: u8, clip: Option<u16>, tick: u64, pos: u64) {
        let Some(id) = self.instruments.iter().find(|i| i.slot == slot).map(|i| i.id.clone()) else { return };
        let t = self.track(&id);
        let clip = clip.and_then(|e| t.clips.iter().find(|(_, c)| c.engine == e).map(|(n, _)| *n));
        if t.playing == clip.map(|n| (n, tick)) {
            return;
        }
        let recording = self.playing && self.starting == 0 && self.take.as_ref().is_some_and(|t| t.slot == slot);
        let pos = pos as f64;
        if recording {
            self.write_take(tick as f64, pos, true);
        }
        self.tracks.get_mut(&id).expect("found").playing = clip.map(|n| (n, tick));
        if recording && let Some(t) = self.take.as_mut() {
            t.take = record::Take::new(tick as f64, pos);
        }
        let track = self.track_info(&id);
        self.emit("engine", Event::Track { track });
        // The Block edits what the armed track plays.
        self.refresh_controller("engine", false);
    }

    /// Where a track is playing now (while playing): its launched clip, from
    /// where it started, else the placement under the song position.
    pub(super) fn track_playhead(&self, id: &str) -> Option<ClipPlayhead> {
        if !self.playing || self.starting > 0 {
            return None;
        }
        let t = self.track(id);
        match t.playing {
            Some((n, since)) => {
                let len = self.clip_len(id, n).max(1) as u64;
                Some(ClipPlayhead { clip: n, tick: (self.play_tick.saturating_sub(since) % len) as u32 })
            }
            None => {
                let pos = self.song_pos?;
                let p = t.placements.iter().find(|p| p.start <= pos && pos < p.end())?;
                let len = self.clip_len(id, p.clip).max(1);
                Some(ClipPlayhead { clip: p.clip, tick: (pos - p.start + p.offset) % len })
            }
        }
    }

    /// Every track's playhead, by instrument.
    pub(super) fn playheads(&self) -> BTreeMap<String, ClipPlayhead> {
        self.instruments.iter().filter_map(|i| Some((i.id.clone(), self.track_playhead(&i.id)?))).collect()
    }

    /// The span the transport loops, as the engine sees it.
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
        // A placement loops its clip, so an offset past the longest clip
        // (the right part of a long one that was cut) is the same as its
        // remainder at the clip's length.
        for p in &mut placements {
            if p.offset >= MAX_CLIP_TICKS && self.track(id).clips.contains_key(&p.clip) {
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
            Request::SongBack(p) => (|| {
                // Only tracks with a launch (a switch would end the notes of
                // one already playing its arrangement).
                let named = p.instrument.as_deref().map(|id| self.find_instrument(id).map(|i| i.id.clone())).transpose()?;
                let ids: Vec<String> = self
                    .instruments
                    .iter()
                    .filter(|i| named.as_ref().is_none_or(|n| *n == i.id))
                    .filter(|i| {
                        let t = self.track(&i.id);
                        t.launched.is_some() || t.playing.is_some()
                    })
                    .map(|i| i.id.clone())
                    .collect();
                self.ensure_room(ids.len())?;
                for id in &ids {
                    self.launch(id, None, p.now.unwrap_or(false))?;
                    self.launch_changed(id, origin);
                }
                Ok(self.song_info())
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
