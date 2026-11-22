//! Samplers' samples (RFC 0010, regions and formats RFC 0011). A pad plays
//! a region of an audio file on the engine host: the file is decoded once
//! into the source cache (`sources.rs`), and the pad gets a copy of just
//! its region, handed to the sampler as a shared buffer
//! (`Command::SetSample`); the old one comes back from the audio thread to
//! be dropped here. A client on another machine sends the file first, in
//! chunks (`sample.upload`); it is stored under `<data dir>/samples/`, and
//! a load naming the upload is journaled as the load of that file, so a
//! replay needs no upload.
//!
//! Saving a project puts each sample in the bundle (`samples/`): all of a
//! WAV file is copied; a region (or another format) is written as a WAV of
//! just that region, with where it came from (`origin`) so it can be edited
//! again while that file is still there. A bundle is self-contained.

use super::sources::Source;
use super::*;
use std::sync::Arc;

pub(super) struct SampleState {
    pub path: PathBuf,
    /// The region of the file the pad plays, in seconds.
    pub start: f64,
    pub end: f64,
    pub file_seconds: f64,
    pub frames: usize,
    pub data: Arc<[f32]>,
    /// Where a bundle's copy came from.
    pub origin: Option<SampleOrigin>,
}

impl SampleState {
    /// Whether the pad plays all of its file.
    fn whole(&self) -> bool {
        self.start <= 1e-9 && (self.end - self.file_seconds).abs() < 1e-6
    }
}

/// Unfinished uploads at once (each holds a part file), and finished ones
/// remembered (for a `sampler.load` naming one).
const MAX_OPEN_UPLOADS: usize = 8;
const MAX_DONE_UPLOADS: usize = 64;

/// An upload in progress, or done (`path`).
pub(super) struct Upload {
    name: String,
    part: PathBuf,
    received: u64,
    path: Option<PathBuf>,
    /// When it started (`Core::upload_clock`), so the oldest goes first.
    started: u64,
}

/// A pad's sample as an undo value: `{path, start, end, origin?}` (a plain
/// path, as before regions, is all of that file).
fn sample_value(s: &SampleState) -> Value {
    let mut v = json!({ "path": s.path.to_string_lossy(), "start": s.start, "end": s.end });
    if let Some(o) = &s.origin {
        v["origin"] = json!(o);
    }
    v
}

impl Core {
    pub(super) fn sampler_info(&self, id: &str) -> SamplerInfo {
        let pads = self
            .samples
            .get(id)
            .map(|pads| {
                pads.iter()
                    .map(|(pad, s)| SamplerPad {
                        pad: *pad,
                        // A bundle's copy shows its original name.
                        name: {
                            let file = s.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let prefix = format!("{id}.{pad}.");
                            file.strip_prefix(&prefix).map(str::to_string).unwrap_or(file)
                        },
                        path: s.path.to_string_lossy().into_owned(),
                        start: s.start,
                        end: s.end,
                        seconds: s.frames as f64 / self.sample_rate.max(1) as f64,
                        file_seconds: s.file_seconds,
                        origin: s.origin.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        SamplerInfo { instrument: id.to_string(), pads }
    }

    pub(super) fn sampler_infos(&self) -> Vec<SamplerInfo> {
        self.instruments.iter().filter(|i| i.kind == InstrumentType::Sampler).map(|i| self.sampler_info(&i.id)).collect()
    }

    fn check_pad(pad: u32) -> Result<(), RpcError> {
        if !(1..=SAMPLER_PADS).contains(&pad) {
            return Err(RpcError::invalid(format!("pad must be 1..{SAMPLER_PADS}")));
        }
        Ok(())
    }

    /// A decoded file, from the cache.
    fn source(&mut self, path: &Path) -> Result<Arc<Source>, RpcError> {
        self.sources.get(path).map_err(RpcError::invalid)
    }

    /// A pad's sample: `start..end` seconds of a file (default: all of
    /// it, if it is not longer than a pad plays), copied out of its
    /// decoded source.
    pub(super) fn cut_sample(
        &mut self,
        path: &Path,
        start: Option<f64>,
        end: Option<f64>,
        origin: Option<SampleOrigin>,
    ) -> Result<SampleState, RpcError> {
        let source = self.source(path)?;
        let file_seconds = source.seconds();
        if start.is_none() && end.is_none() && file_seconds > sources::MAX_REGION_SECONDS {
            return Err(RpcError::invalid(format!(
                "{} is {file_seconds:.1} s; a pad plays at most {} s: load a region of it (start, end)",
                path.display(),
                sources::MAX_REGION_SECONDS
            )));
        }
        let (start, end) = (start.unwrap_or(0.0), end.unwrap_or(file_seconds));
        let (a, b) = source.frame_range(start, end).map_err(RpcError::invalid)?;
        // Where the region really is (rounded to frames, and not past the end).
        let rate = source.rate as f64;
        let (start, end) = if end >= file_seconds { (a as f64 / rate, file_seconds) } else { (a as f64 / rate, b as f64 / rate) };
        Ok(SampleState { path: path.to_path_buf(), start, end, file_seconds, frames: b - a, data: source.region(a, b), origin })
    }

    /// Give a pad a sample (or none), telling the engine; the old one comes
    /// back to be dropped. The caller checked the instrument is a sampler
    /// and queue room.
    pub(super) fn set_sample_unchecked(&mut self, id: &str, pad: u32, sample: Option<SampleState>) {
        let Some(slot) = self.slot_of(id) else { return };
        let had = self.samples.get(id).is_some_and(|p| p.contains_key(&pad));
        self.send(Command::SetSample { slot, pad: (pad - 1) as u8, sample: sample.as_ref().map(|s| s.data.clone()) });
        if had {
            self.in_flight += 1;
        }
        let pads = self.samples.entry(id.to_string()).or_default();
        match sample {
            Some(s) => {
                pads.insert(pad, s);
            }
            None => {
                pads.remove(&pad);
            }
        }
    }

    /// Load (a region of) a file on the engine host into a pad.
    pub(super) fn load_sample(
        &mut self,
        id: &str,
        pad: u32,
        path: &Path,
        region: (Option<f64>, Option<f64>),
        origin: Option<SampleOrigin>,
        by: &str,
    ) -> Result<SamplerInfo, RpcError> {
        Self::check_pad(pad)?;
        if self.in_flight >= RETURN_CAPACITY {
            return Err(RpcError::failed("the audio engine is still returning removed samples; try again"));
        }
        self.ensure_room(1)?;
        let sample = self.cut_sample(path, region.0, region.1, origin)?;
        self.set_sample_unchecked(id, pad, Some(sample));
        self.sampler_changed(id, by);
        Ok(self.sampler_info(id))
    }

    fn sampler_changed(&mut self, id: &str, origin: &str) {
        let info = self.sampler_info(id);
        self.emit(origin, Event::Sampler { info });
        self.mark_dirty(origin);
    }

    /// Where an uploaded file is stored: `<data dir>/samples/<name>`, or
    /// that file if it holds the same bytes already (sending a file again
    /// keeps one copy), else numbered.
    pub(super) fn upload_path(&self, name: &str, same_as: Option<&Path>) -> PathBuf {
        let clean: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
            .collect();
        let clean = if clean.trim_matches('.').is_empty() { "sample.wav".to_string() } else { clean };
        let dir = self.data_dir.join("samples");
        let mut path = dir.join(&clean);
        let mut n = 2;
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
        let same = |p: &Path| {
            same_as.is_some_and(|q| size(p).is_some() && size(p) == size(q) && std::fs::read(p).ok() == std::fs::read(q).ok())
        };
        while path.exists() {
            if same(&path) {
                return path;
            }
            path = dir.join(format!("{n}-{clean}"));
            n += 1;
        }
        path
    }

    /// A chunk of an upload. The first (offset 0) starts it; each next one
    /// goes where the last ended; the last (`done`) is checked (it must
    /// decode) and stored. A refused upload leaves nothing behind.
    pub(super) fn upload_chunk(&mut self, p: &SampleUploadParams) -> Result<SampleUploadResult, RpcError> {
        let token = &p.upload;
        if token.is_empty() || token.len() > 64 || !token.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')) {
            return Err(RpcError::invalid("an upload id is 1..64 letters, digits, '-' or '_'"));
        }
        let bytes = base64_decode(&p.data).map_err(RpcError::invalid)?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(RpcError::invalid(format!("an upload chunk is at most {} MB", MAX_UPLOAD_BYTES / 1024 / 1024)));
        }
        let dir = self.data_dir.join("uploads");
        if p.offset == 0 {
            // (Re)start it. The part keeps the name's extension, which
            // tells the decoder the format.
            if let Some(old) = self.uploads.remove(token) {
                std::fs::remove_file(old.part).ok();
            }
            let ext = Path::new(&p.name).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            let part = dir.join(format!("{token}{ext}"));
            std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::write(&part, []))
                .map_err(|e| RpcError::failed(format!("store {}: {e}", part.display())))?;
            // The oldest unfinished upload goes when too many are open (one
            // abandoned by a client that went away); the oldest finished one
            // is forgotten past a limit (its file stays).
            self.upload_clock += 1;
            let open: Vec<(String, u64)> = self.uploads.iter().filter(|(_, u)| u.path.is_none()).map(|(t, u)| (t.clone(), u.started)).collect();
            if open.len() >= MAX_OPEN_UPLOADS
                && let Some((oldest, _)) = open.iter().min_by_key(|(_, at)| *at)
                && let Some(old) = self.uploads.remove(oldest)
            {
                std::fs::remove_file(old.part).ok();
            }
            let done: Vec<(String, u64)> = self.uploads.iter().filter(|(_, u)| u.path.is_some()).map(|(t, u)| (t.clone(), u.started)).collect();
            if done.len() >= MAX_DONE_UPLOADS
                && let Some((oldest, _)) = done.iter().min_by_key(|(_, at)| *at)
            {
                self.uploads.remove(oldest);
            }
            let started = self.upload_clock;
            self.uploads.insert(token.clone(), Upload { name: p.name.clone(), part, received: 0, path: None, started });
        }
        let up = self.uploads.get_mut(token).ok_or_else(|| RpcError::invalid(format!("no upload '{token}' (start one at offset 0)")))?;
        if up.path.is_some() {
            return Err(RpcError::invalid(format!("upload '{token}' is finished")));
        }
        if p.offset != up.received {
            return Err(RpcError::invalid(format!("upload '{token}' has {} bytes; the next chunk goes there", up.received)));
        }
        if up.received + bytes.len() as u64 > MAX_UPLOAD_TOTAL_BYTES {
            let part = up.part.clone();
            self.uploads.remove(token);
            std::fs::remove_file(part).ok();
            return Err(RpcError::invalid(format!("uploads are at most {} MB", MAX_UPLOAD_TOTAL_BYTES / 1024 / 1024)));
        }
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&up.part)
                .map_err(|e| RpcError::failed(format!("store {}: {e}", up.part.display())))?;
            f.write_all(&bytes).map_err(|e| RpcError::failed(format!("store {}: {e}", up.part.display())))?;
        }
        up.received += bytes.len() as u64;
        let (received, part, name) = (up.received, up.part.clone(), up.name.clone());
        if !p.done {
            return Ok(SampleUploadResult { upload: token.clone(), received, path: None });
        }
        // Done: it must be audio this build can play, then it is stored
        // (decoded now, so a load of it right after finds it cached).
        let checked = self.sources.get(&part);
        self.sources.forget(&part);
        if let Err(e) = checked {
            self.uploads.remove(token);
            std::fs::remove_file(&part).ok();
            // Named as sent (not as the temporary part).
            let why = e.strip_prefix(&format!("{}: ", part.display())).unwrap_or(&e);
            return Err(RpcError::invalid(format!("{name}: {why}")));
        }
        let path = self.upload_path(&name, Some(&part));
        if path.exists() {
            std::fs::remove_file(&part).ok();
        } else {
            std::fs::create_dir_all(path.parent().expect("under the data dir"))
                .and_then(|_| std::fs::rename(&part, &path))
                .map_err(|e| RpcError::failed(format!("store {}: {e}", path.display())))?;
        }
        if let Some(up) = self.uploads.get_mut(token) {
            up.path = Some(path.clone());
        }
        Ok(SampleUploadResult { upload: token.clone(), received, path: Some(path.to_string_lossy().into_owned()) })
    }

    /// A file a request names: a path on the engine host (relative: under
    /// the data dir), or `upload:<id>`, a finished upload.
    fn sample_path(&self, path: &str) -> Result<PathBuf, RpcError> {
        match path.strip_prefix("upload:") {
            Some(token) => self.uploaded(token),
            None => Ok(self.resolve_data_path(path)),
        }
    }

    /// The file a finished upload was stored as.
    pub(super) fn uploaded(&self, token: &str) -> Result<PathBuf, RpcError> {
        self.uploads
            .get(token)
            .and_then(|u| u.path.clone())
            .ok_or_else(|| RpcError::invalid(format!("no finished upload '{token}'")))
    }

    /// Sampler and sample RPCs. Any other request is handed back.
    pub(super) fn handle_sampler(&mut self, req: Request, origin: &str) -> Result<RpcResult, Request> {
        let sampler = |c: &Self, id: &Option<String>| c.resolve(id.as_deref(), InstrumentType::Sampler);
        Ok(match req {
            Request::SamplerGet(p) => sampler(self, &p.instrument).map(|id| self.sampler_info(&id)).and_then(ok),
            Request::SamplerLoad(p) => (|| {
                let id = sampler(self, &p.instrument)?;
                let path = match (&p.path, &p.upload) {
                    (Some(path), None) => self.resolve_data_path(path),
                    (None, Some(token)) => self.uploaded(token)?,
                    _ => return Err(RpcError::invalid("give a path or an upload")),
                };
                self.load_sample(&id, p.pad, &path, (p.start, p.end), None, origin)
            })()
            .and_then(ok),
            Request::SampleUpload(p) => self.upload_chunk(&p).and_then(ok),
            Request::SampleInfo(p) => (|| {
                let path = self.sample_path(&p.path)?;
                let s = self.source(&path)?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                Ok(SampleInfo { path: path.to_string_lossy().into_owned(), name, seconds: s.seconds(), format: s.format.clone() })
            })()
            .and_then(ok),
            Request::SamplePeaks(p) => (|| {
                if !(1..=4096).contains(&p.bins) {
                    return Err(RpcError::invalid("bins must be 1..4096"));
                }
                let path = self.sample_path(&p.path)?;
                let s = self.source(&path)?;
                let (start, end) = (p.start.unwrap_or(0.0).max(0.0), p.end.unwrap_or(s.seconds()).min(s.seconds()));
                if end <= start {
                    return Err(RpcError::invalid("the span runs from a start to a later end, in seconds"));
                }
                Ok(SamplePeaks { start, end, peaks: s.peaks(start, end, p.bins as usize) })
            })()
            .and_then(ok),
            Request::SamplerClear(p) => (|| {
                let id = sampler(self, &p.instrument)?;
                Self::check_pad(p.pad)?;
                if self.samples.get(&id).is_some_and(|s| s.contains_key(&p.pad)) {
                    if self.in_flight >= RETURN_CAPACITY {
                        return Err(RpcError::failed("the audio engine is still returning removed samples; try again"));
                    }
                    self.ensure_room(1)?;
                    self.set_sample_unchecked(&id, p.pad, None);
                    self.sampler_changed(&id, origin);
                }
                Ok(self.sampler_info(&id))
            })()
            .and_then(ok),
            other => return Err(other),
        })
    }

    /// Samples as undo keys: `sample:<id>.<pad>` -> `{path, start, end,
    /// origin?}`.
    pub(super) fn sample_doc(&self, d: &mut Doc) {
        for (id, pads) in &self.samples {
            for (pad, s) in pads {
                d.insert(format!("sample:{id}.{pad}"), sample_value(s));
            }
        }
    }

    /// Apply undo sets of `sample:` keys (after instruments exist). Returns
    /// keys that could not be set.
    pub(super) fn apply_sample_sets(&mut self, sets: Vec<(String, u32, Value)>, origin: &str) -> Vec<String> {
        let mut skipped = Vec::new();
        for (id, pad, v) in sets {
            let key = format!("sample:{id}.{pad}");
            let sampler = self.instruments.iter().any(|i| i.id == id && i.kind == InstrumentType::Sampler);
            let path = v.as_str().or_else(|| v.get("path").and_then(Value::as_str)).map(PathBuf::from);
            let region = (v.get("start").and_then(Value::as_f64), v.get("end").and_then(Value::as_f64));
            let from = v.get("origin").and_then(|o| serde_json::from_value(o.clone()).ok());
            let result = match (&path, sampler) {
                (_, false) => Err(()),
                (Some(p), true) => self.load_sample(&id, pad, p, region, from, origin).map(|_| ()).map_err(|_| ()),
                (None, true) if self.in_flight < RETURN_CAPACITY => {
                    self.set_sample_unchecked(&id, pad, None);
                    self.sampler_changed(&id, origin);
                    Ok(())
                }
                (None, true) => Err(()),
            };
            if result.is_err() {
                skipped.push(key);
            }
        }
        skipped
    }

    /// The project's samples, as `ProjectFile::samples` (paths on the
    /// engine host): a path for all of a file, else its region.
    pub(super) fn project_samples(&self) -> BTreeMap<String, BTreeMap<u32, ProjectSample>> {
        self.samples
            .iter()
            .filter(|(_, pads)| !pads.is_empty())
            .map(|(id, pads)| {
                let pads = pads
                    .iter()
                    .map(|(n, s)| {
                        let path = s.path.to_string_lossy().into_owned();
                        let entry = if s.whole() && s.origin.is_none() {
                            ProjectSample::Path(path)
                        } else {
                            let (start, end) = if s.whole() { (None, None) } else { (Some(s.start), Some(s.end)) };
                            ProjectSample::Region(ProjectSampleRegion { path, start, end, origin: s.origin.clone() })
                        };
                        (*n, entry)
                    })
                    .collect();
                (id.clone(), pads)
            })
            .collect()
    }

    /// Put every sample in a bundle's `samples/` and point `file` at it,
    /// relative to the bundle: all of a WAV file is copied (one already in
    /// it stays where it is); a region, or another format, is written as a
    /// WAV of the region with where it came from.
    pub(super) fn bundle_samples(&self, bundle: &Path, file: &mut ProjectFile) -> Result<(), RpcError> {
        let dir = bundle.join("samples");
        for (id, pads) in &self.samples {
            for (pad, s) in pads {
                let entry = match self.bundle_sample(bundle, &dir, id, *pad, s)? {
                    (rel, None) => ProjectSample::Path(rel),
                    (rel, origin) => ProjectSample::Region(ProjectSampleRegion { path: rel, start: None, end: None, origin }),
                };
                file.samples.entry(id.clone()).or_default().insert(*pad, entry);
            }
        }
        Ok(())
    }

    /// Put one pad's sample in the bundle: its path there, and where it
    /// came from if it is a cut.
    fn bundle_sample(&self, bundle: &Path, dir: &Path, id: &str, pad: u32, s: &SampleState) -> Result<(String, Option<SampleOrigin>), RpcError> {
        let wav = s.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav"));
        if let Ok(inside) = s.path.strip_prefix(dir)
            && s.whole()
        {
            return Ok((format!("samples/{}", inside.to_string_lossy()), s.origin.clone()));
        }
        let file_name = s.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "sample.wav".into());
        // From another bundle: its `<id>.<pad>.` prefix does not stack.
        let prefix = format!("{id}.{pad}.");
        let name = file_name.strip_prefix(&prefix).map(str::to_string).unwrap_or(file_name);
        if s.whole() && wav {
            // `<id>.<pad>.<file>`: ids have no dots, so names never clash.
            let rel = format!("samples/{id}.{pad}.{name}");
            let dst = bundle.join(&rel);
            // The original may be gone since the last save: the copy in
            // this bundle (or the project's current one) stands in.
            let current = self.project.path.as_ref().map(|b| PathBuf::from(b).join(&rel));
            let src = if s.path.exists() {
                s.path.clone()
            } else if dst.exists() {
                dst.clone()
            } else {
                current.filter(|c| c.exists()).unwrap_or(s.path.clone())
            };
            if src != dst {
                std::fs::create_dir_all(dir)
                    .and_then(|_| std::fs::copy(&src, &dst))
                    .map_err(|e| RpcError::failed(format!("copy {} into the bundle: {e}", src.display())))?;
            }
            return Ok((rel, s.origin.clone()));
        }
        // A cut (or a whole file in another format): a WAV of what the pad
        // plays, at the engine's rate.
        let stem = Path::new(&name).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "sample".into());
        // Written beside the bundle's files first, then named: the same name
        // if that file holds the same audio (saving again), else a fresh one,
        // so a file the pad cuts from, or an undo step loads, is never
        // overwritten.
        std::fs::create_dir_all(dir).map_err(|e| RpcError::failed(format!("{}: {e}", dir.display())))?;
        let tmp = dir.join(format!(".{id}.{pad}.saving.wav"));
        write_wav(&tmp, &s.data, self.sample_rate).map_err(|e| RpcError::failed(format!("write {}: {e}", tmp.display())))?;
        let mut rel = format!("samples/{id}.{pad}.{stem}.wav");
        let mut n = 2;
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
        let same = |p: &Path| size(p) == size(&tmp) && std::fs::read(p).ok() == std::fs::read(&tmp).ok();
        while bundle.join(&rel).exists() && !same(&bundle.join(&rel)) {
            rel = format!("samples/{id}.{pad}.{stem}-{n}.wav");
            n += 1;
        }
        let dst = bundle.join(&rel);
        std::fs::rename(&tmp, &dst).map_err(|e| RpcError::failed(format!("write {}: {e}", dst.display())))?;
        // Where it came from: a part of a bundle copy is that part of the
        // copy's own origin (the copy is that region, at the same pace).
        let origin = match &s.origin {
            Some(o) if s.whole() => o.clone(),
            Some(o) => SampleOrigin { path: o.path.clone(), start: o.start + s.start, end: (o.start + s.end).min(o.end) },
            None => {
                let path = std::path::absolute(&s.path).unwrap_or_else(|_| s.path.clone()).to_string_lossy().into_owned();
                SampleOrigin { path, start: s.start, end: s.end }
            }
        };
        let origin = Some(origin);
        Ok((rel, origin))
    }
}

/// Write interleaved stereo as a 32-bit float WAV.
fn write_wav(path: &Path, data: &[f32], rate: u32) -> Result<(), String> {
    let spec = hound::WavSpec { channels: 2, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    for v in data {
        w.write_sample(*v).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}

/// Resolve a project file's sample paths against its bundle (relative
/// ones) before it is applied.
pub(super) fn resolve_samples(bundle: &Path, file: &mut ProjectFile) {
    for pads in file.samples.values_mut() {
        for entry in pads.values_mut() {
            let path = entry.path_mut();
            if Path::new(path.as_str()).is_relative() {
                *path = bundle.join(&*path).to_string_lossy().into_owned();
            }
        }
    }
}
