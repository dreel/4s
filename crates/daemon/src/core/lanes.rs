//! Automation lanes (RFC 0010 phase 6): a clip's lanes move parameters
//! while it plays. Each lane names a continuous or whole-number parameter
//! (not the transport's) and holds up to `MAX_LANE_POINTS` points, linear
//! between them. The engine plays them (`Engine::automate`); this side
//! checks them, keeps them in each clip, and tells the engine which
//! parameter each lane moves, again when a graph change moves parameters
//! to other engine slots. A lane whose parameter goes (its instrument or
//! device removed) goes with it, as modulation routes do.

use super::*;

/// Engine commands a clip's lanes take at most when sent.
pub(super) fn lane_commands(lanes: &[Lane]) -> usize {
    MAX_LANES + lanes.iter().map(|l| l.points.len()).sum::<usize>()
}

/// A lane's points sorted by tick (a later point at the same tick wins),
/// checked.
fn normalize_points(points: &[LanePoint]) -> Result<Vec<LanePoint>, String> {
    let mut out: Vec<LanePoint> = Vec::with_capacity(points.len());
    for p in points {
        if !p.value.is_finite() {
            return Err("point values must be finite numbers".into());
        }
        if p.tick >= MAX_CLIP_TICKS {
            return Err(format!("point ticks must be 0..{}", MAX_CLIP_TICKS - 1));
        }
        out.retain(|q| q.tick != p.tick);
        out.push(*p);
    }
    out.sort_by_key(|p| p.tick);
    if out.len() > MAX_LANE_POINTS {
        return Err(format!("at most {MAX_LANE_POINTS} points in a lane"));
    }
    Ok(out)
}

impl Core {
    /// A lane's parameter: its engine target, range, and registry index.
    /// Continuous or whole-number, and not the transport's (the daemon
    /// counts time by it).
    pub(super) fn lane_target(&self, path: &str) -> Result<(ParamTarget, f32, f32, usize), RpcError> {
        if transport_param(path) {
            return Err(RpcError::invalid(format!("'{path}' is the transport's: it takes no automation")));
        }
        let id = self.param_id(path)?;
        let p = &self.params[id];
        match p.info.kind {
            ParamKind::Continuous { min, max } => Ok((p.target, min as f32, max as f32, id)),
            ParamKind::Integer { min, max } => Ok((p.target, min as f32, max as f32, id)),
            ParamKind::Toggle => Err(RpcError::invalid(format!("'{path}' is a toggle: only continuous and whole-number parameters take automation"))),
        }
    }

    /// A lane with its points sorted and clamped (and rounded, for a
    /// whole-number parameter) to its parameter's range, or why not.
    pub(super) fn check_lane(&self, l: Lane) -> Result<Lane, RpcError> {
        let (.., pid) = self.lane_target(&l.path)?;
        let mut points = normalize_points(&l.points).map_err(|e| RpcError::invalid(format!("{}: {e}", l.path)))?;
        if points.is_empty() {
            return Err(RpcError::invalid(format!("{}: a lane has at least one point", l.path)));
        }
        for p in &mut points {
            p.value = self.params[pid].info.clamp(p.value);
        }
        Ok(Lane { path: l.path, points })
    }

    /// Replace a clip's lanes: checked (known parameters, one lane each, at
    /// most `MAX_LANES`), points sorted and clamped to their parameter's
    /// range, then sent to the engine. Emits `clip_changed`.
    pub(super) fn set_lanes(&mut self, id: &str, n: u32, lanes: Vec<Lane>, origin: &str) -> Result<Clip, RpcError> {
        let n = self.clip_id(id, Some(n))?;
        if lanes.len() > MAX_LANES {
            return Err(RpcError::invalid(format!("at most {MAX_LANES} lanes in a clip")));
        }
        let mut checked: Vec<Lane> = Vec::with_capacity(lanes.len());
        for l in lanes {
            if checked.iter().any(|c| c.path == l.path) {
                return Err(RpcError::invalid(format!("two lanes for '{}'", l.path)));
            }
            checked.push(self.check_lane(l)?);
        }
        if self.track(id).clips[&n].lanes == checked {
            // The same lanes again still repair any the engine did not get.
            self.sync_lanes(id, n, false);
            return Ok(self.clip(id, n));
        }
        self.ensure_room(lane_commands(&checked))?;
        self.tracks.get_mut(id).and_then(|t| t.clips.get_mut(&n)).expect("checked").lanes = checked;
        self.sync_lanes(id, n, true);
        let clip = self.clip(id, n);
        self.emit(origin, Event::ClipChanged { clip: clip.clone() });
        self.mark_dirty(origin);
        Ok(clip)
    }

    /// Tell the engine a clip's lanes: with `points`, every lane and its
    /// points (after an edit); else only lanes whose target moved (after a
    /// graph change), keeping the points it has.
    fn sync_lanes(&mut self, id: &str, n: u32, points: bool) {
        let c = &self.track(id).clips[&n];
        let (clip, mut sent) = (c.engine, c.sent);
        // Per lane: what it will be once its commands are queued.
        let mut commands: Vec<(usize, Option<(ParamTarget, u8)>, Vec<Command>)> = Vec::new();
        for (k, sent) in sent.iter_mut().enumerate() {
            let mut lane_commands = Vec::new();
            let lane = k as u8;
            let want = c.lanes.get(k).and_then(|l| self.lane_target(&l.path).ok().map(|t| (t, l)));
            let key = want.map(|((target, ..), l)| (target, l.points.len() as u8));
            if key == *sent && (!points || key.is_none()) {
                continue;
            }
            match want {
                Some(((target, min, max, pid), l)) => {
                    let base = self.params[pid].value as f32;
                    let whole = matches!(self.params[pid].info.kind, ParamKind::Integer { .. });
                    let len = l.points.len() as u8;
                    lane_commands.push(Command::SetLane { clip, lane, target: Some(target), base, min, max, whole, len });
                    if points || sent.is_none_or(|s| s.1 != len) {
                        for (i, p) in l.points.iter().enumerate() {
                            lane_commands.push(Command::SetLanePoint { clip, lane, index: i as u8, tick: p.tick, value: p.value as f32 });
                        }
                    }
                }
                None => lane_commands.push(Command::SetLane { clip, lane, target: None, base: 0.0, min: 0.0, max: 1.0, whole: false, len: 0 }),
            }
            commands.push((k, key, lane_commands));
        }
        // A lane whose commands did not all fit is not marked sent (a lane
        // that did not get its points is marked unsent), so the next sync
        // sends it again instead of trusting the engine has it. Until then
        // (the next edit or graph change) it may play stale points; callers'
        // `ensure_room` makes a full queue here unlikely.
        for (k, key, lane_commands) in commands {
            let mut queued = true;
            for command in lane_commands {
                if let Err(rtrb::PushError::Full(command)) = self.commands.push(command) {
                    tracing::warn!("engine command queue full; dropped {command:?}");
                    queued = false;
                }
            }
            sent[k] = if queued { key } else if key.is_some() { None } else { sent[k] };
        }
        self.tracks.get_mut(id).and_then(|t| t.clips.get_mut(&n)).expect("checked").sent = sent;
    }

    /// After a graph change: drop lanes whose parameter went, and retarget
    /// the rest in the engine (parameters may have moved slots).
    pub(super) fn refresh_lanes(&mut self, origin: &str) {
        let clips: Vec<(String, u32)> = self
            .tracks
            .iter()
            .flat_map(|(id, t)| {
                t.clips.iter().filter(|(_, c)| !c.lanes.is_empty() || c.sent.iter().any(Option::is_some)).map(move |(n, _)| (id.clone(), *n))
            })
            .collect();
        for (id, n) in clips {
            let c = &self.track(&id).clips[&n];
            let keep: Vec<Lane> = c.lanes.iter().filter(|l| self.lane_target(&l.path).is_ok()).cloned().collect();
            if keep.len() == c.lanes.len() {
                self.sync_lanes(&id, n, false);
            } else {
                self.tracks.get_mut(&id).and_then(|t| t.clips.get_mut(&n)).expect("listed").lanes = keep;
                self.sync_lanes(&id, n, true);
                let clip = self.clip(&id, n);
                self.emit(origin, Event::ClipChanged { clip });
            }
        }
    }

    /// A clip's lanes as the offline renderer takes them.
    pub(super) fn render_lanes(&self, c: &clips::ClipState) -> Vec<fours_engine::offline::RenderLane> {
        c.lanes
            .iter()
            .filter_map(|l| {
                let (target, min, max, pid) = self.lane_target(&l.path).ok()?;
                let points = l.points.iter().map(|p| (p.tick, p.value as f32)).collect();
                let whole = matches!(self.params[pid].info.kind, ParamKind::Integer { .. });
                Some(fours_engine::offline::RenderLane { target, base: self.params[pid].value as f32, min, max, whole, points })
            })
            .collect()
    }
}
