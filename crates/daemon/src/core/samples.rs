//! Samplers' samples (RFC 0010). A WAV file on the engine host is decoded
//! here (8/16/24/32-bit integer or 32-bit float, mono or stereo), resampled
//! to the engine's rate, and handed to the sampler's pad as a shared buffer
//! (`Command::SetSample`); the old one comes back from the audio thread to
//! be dropped here. A client on another machine uploads the file
//! (`sampler.upload`); it is stored under `<data dir>/samples/` and
//! journaled as the load of that file, so a replay needs no upload.
//!
//! Saving a project copies each sample into the bundle (`samples/`) and
//! stores the path inside it; loading resolves those paths against the
//! bundle, so a bundle is self-contained.

use super::*;
use std::sync::Arc;

/// Longest sample: 60 seconds at the engine's rate.
const MAX_SECONDS: f64 = 60.0;

pub(super) struct SampleState {
    pub path: PathBuf,
    pub frames: usize,
    pub data: Arc<[f32]>,
}

/// Decode a WAV file to interleaved stereo at `rate`.
pub(super) fn decode(path: &Path, rate: u32) -> Result<(Arc<[f32]>, usize), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode_from(std::io::BufReader::new(file), rate).map_err(|e| format!("{}: {e}", path.display()))
}

/// Decode WAV data to interleaved stereo at `rate`.
pub(super) fn decode_from<R: std::io::Read>(reader: R, rate: u32) -> Result<(Arc<[f32]>, usize), String> {
    let mut r = hound::WavReader::new(reader).map_err(|e| e.to_string())?;
    let spec = r.spec();
    let channels = spec.channels.max(1) as usize;
    if r.duration() as f64 / spec.sample_rate as f64 > MAX_SECONDS {
        return Err(format!("longer than {MAX_SECONDS} s"));
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()
        }
    }
    .map_err(|e| e.to_string())?;
    // Stereo frames at the file's rate (mono doubled, extra channels left out).
    let frames = samples.len() / channels;
    let at = |f: usize, c: usize| samples[f * channels + c.min(channels - 1)];
    // Linear resampling to the engine's rate.
    let ratio = spec.sample_rate as f64 / rate as f64;
    let out_frames = ((frames as f64) / ratio).floor() as usize;
    if out_frames < 2 {
        return Err("no audio".into());
    }
    let mut out = Vec::with_capacity(out_frames * 2);
    for i in 0..out_frames {
        let pos = i as f64 * ratio;
        let k = (pos as usize).min(frames - 1);
        let t = (pos - k as f64) as f32;
        let k1 = (k + 1).min(frames - 1);
        for c in 0..2 {
            out.push(at(k, c) * (1.0 - t) + at(k1, c) * t);
        }
    }
    Ok((out.into(), out_frames))
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
                        seconds: s.frames as f64 / self.sample_rate.max(1) as f64,
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

    /// Load a file on the engine host into a pad.
    pub(super) fn load_sample(&mut self, id: &str, pad: u32, path: &Path, origin: &str) -> Result<SamplerInfo, RpcError> {
        Self::check_pad(pad)?;
        if self.in_flight >= RETURN_CAPACITY {
            return Err(RpcError::failed("the audio engine is still returning removed samples; try again"));
        }
        self.ensure_room(1)?;
        let (data, frames) = decode(path, self.sample_rate).map_err(RpcError::invalid)?;
        self.set_sample_unchecked(id, pad, Some(SampleState { path: path.to_path_buf(), frames, data }));
        self.sampler_changed(id, origin);
        Ok(self.sampler_info(id))
    }

    fn sampler_changed(&mut self, id: &str, origin: &str) {
        let info = self.sampler_info(id);
        self.emit(origin, Event::Sampler { info });
        self.mark_dirty(origin);
    }

    /// Where an uploaded file is stored: `<data dir>/samples/<name>`, numbered
    /// if taken.
    pub(super) fn upload_path(&self, name: &str) -> PathBuf {
        let clean: String = name
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
            .collect();
        let clean = if clean.trim_matches('.').is_empty() { "sample.wav".to_string() } else { clean };
        let dir = self.data_dir.join("samples");
        let mut path = dir.join(&clean);
        let mut n = 2;
        while path.exists() {
            path = dir.join(format!("{n}-{clean}"));
            n += 1;
        }
        path
    }

    /// Store an upload, returning where it went. Checked first (a sampler,
    /// a pad, a WAV it can play), so a refused upload leaves nothing behind.
    pub(super) fn store_upload(&self, p: &SamplerUploadParams) -> Result<PathBuf, RpcError> {
        self.resolve(p.instrument.as_deref(), InstrumentType::Sampler)?;
        Self::check_pad(p.pad)?;
        let bytes = base64_decode(&p.data).map_err(RpcError::invalid)?;
        if bytes.len() > MAX_UPLOAD_BYTES {
            return Err(RpcError::invalid(format!("uploads are at most {} MB", MAX_UPLOAD_BYTES / 1024 / 1024)));
        }
        decode_from(std::io::Cursor::new(&bytes), self.sample_rate).map_err(|e| RpcError::invalid(format!("{}: {e}", p.name)))?;
        let path = self.upload_path(&p.name);
        std::fs::create_dir_all(path.parent().expect("under the data dir"))
            .and_then(|_| std::fs::write(&path, bytes))
            .map_err(|e| RpcError::failed(format!("store {}: {e}", path.display())))?;
        Ok(path)
    }

    /// Sampler RPCs. Any other request is handed back.
    pub(super) fn handle_sampler(&mut self, req: Request, origin: &str) -> Result<RpcResult, Request> {
        let sampler = |c: &Self, id: &Option<String>| c.resolve(id.as_deref(), InstrumentType::Sampler);
        Ok(match req {
            Request::SamplerGet(p) => sampler(self, &p.instrument).map(|id| self.sampler_info(&id)).and_then(ok),
            Request::SamplerLoad(p) => (|| {
                let id = sampler(self, &p.instrument)?;
                let path = self.resolve_data_path(&p.path);
                self.load_sample(&id, p.pad, &path, origin)
            })()
            .and_then(ok),
            // Normally rewritten to `sampler.load` before it gets here
            // (`Core::handle`); a batch can still hold one.
            Request::SamplerUpload(p) => (|| {
                let id = sampler(self, &p.instrument)?;
                Self::check_pad(p.pad)?;
                let path = self.store_upload(&p)?;
                self.load_sample(&id, p.pad, &path, origin)
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

    /// Samples as undo keys: `sample:<id>.<pad>` -> the file's path.
    pub(super) fn sample_doc(&self, d: &mut Doc) {
        for (id, pads) in &self.samples {
            for (pad, s) in pads {
                d.insert(format!("sample:{id}.{pad}"), json!(s.path.to_string_lossy()));
            }
        }
    }

    /// Apply undo sets of `sample:` keys (after instruments exist). Returns
    /// keys that could not be set.
    pub(super) fn apply_sample_sets(&mut self, sets: Vec<(String, u32, Option<String>)>, origin: &str) -> Vec<String> {
        let mut skipped = Vec::new();
        for (id, pad, path) in sets {
            let key = format!("sample:{id}.{pad}");
            let sampler = self.instruments.iter().any(|i| i.id == id && i.kind == InstrumentType::Sampler);
            let result = match (&path, sampler) {
                (_, false) => Err(()),
                (Some(p), true) => self.load_sample(&id, pad, Path::new(p), origin).map(|_| ()).map_err(|_| ()),
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
    /// engine host).
    pub(super) fn project_samples(&self) -> BTreeMap<String, BTreeMap<u32, String>> {
        self.samples
            .iter()
            .filter(|(_, pads)| !pads.is_empty())
            .map(|(id, pads)| (id.clone(), pads.iter().map(|(n, s)| (*n, s.path.to_string_lossy().into_owned())).collect()))
            .collect()
    }

    /// Copy every sample into a bundle's `samples/` and point `file` at the
    /// copies, relative to the bundle. A sample already in it stays where
    /// it is.
    pub(super) fn bundle_samples(&self, bundle: &Path, file: &mut ProjectFile) -> Result<(), RpcError> {
        let dir = bundle.join("samples");
        for (id, pads) in file.samples.iter_mut() {
            for (pad, path) in pads.iter_mut() {
                let src = PathBuf::from(&*path);
                if let Ok(inside) = src.strip_prefix(&dir) {
                    *path = format!("samples/{}", inside.to_string_lossy());
                    continue;
                }
                let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "sample.wav".into());
                // `<id>.<pad>.<file>`: ids have no dots, so names never clash.
                let rel = format!("samples/{id}.{pad}.{name}");
                let dst = bundle.join(&rel);
                if src != dst {
                    std::fs::create_dir_all(&dir)
                        .and_then(|_| std::fs::copy(&src, &dst))
                        .map_err(|e| RpcError::failed(format!("copy {} into the bundle: {e}", src.display())))?;
                }
                *path = rel;
            }
        }
        Ok(())
    }
}

/// Resolve a project file's sample paths against its bundle (relative
/// ones) before it is applied.
pub(super) fn resolve_samples(bundle: &Path, file: &mut ProjectFile) {
    for pads in file.samples.values_mut() {
        for path in pads.values_mut() {
            if Path::new(path.as_str()).is_relative() {
                *path = bundle.join(&*path).to_string_lossy().into_owned();
            }
        }
    }
}
