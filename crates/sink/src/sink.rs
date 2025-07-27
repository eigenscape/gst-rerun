use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_base::prelude::BaseSinkExtManual;
use gst_base::subclass::prelude::*;
use gst_video::subclass::prelude::*;
use once_cell::sync::Lazy;
use std::sync::Mutex;
use thiserror::Error;

// Debug category for logging
static CAT: Lazy<gst::DebugCategory> = Lazy::new(|| {
    gst::DebugCategory::new(
        "rerunsink",
        gst::DebugColorFlags::empty(),
        Some("Rerun sink"),
    )
});

// Custom error type for frame processing
#[derive(Debug, Error)]
enum RerunSinkError {
    #[error("Failed to map buffer for reading")]
    BufferMapFailed,

    #[error("Failed to log image to rerun: {0}")]
    RerunLogFailed(#[from] rerun::RecordingStreamError),

    #[error("Failed to get video info from caps")]
    VideoInfoFromCapsFailed,

    #[error("Unsupported encoding format: {0}")]
    UnsupportedEncodingFormat(String),

    #[error("Unsupported video format: {0:?}")]
    UnsupportedVideoFormat(gst_video::VideoFormat),
}

// Default property values matching C++ implementation
const DEFAULT_GRPC_ADDRESS: &str = "rerun+http://127.0.0.1:9876/proxy";
const DEFAULT_SPAWN_VIEWER: bool = false;

// Property value storage
#[derive(Debug)]
struct Settings {
    app_id: Option<String>,
    recording_id: Option<String>,
    entity_path: Option<String>,
    spawn_viewer: bool,
    grpc_address: String,
    recorder: Option<rerun::RecordingStream>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            app_id: None,
            recording_id: None,
            entity_path: None,
            spawn_viewer: DEFAULT_SPAWN_VIEWER,
            grpc_address: DEFAULT_GRPC_ADDRESS.to_string(),
            recorder: None,
        }
    }
}

impl Clone for Settings {
    fn clone(&self) -> Self {
        Settings {
            app_id: self.app_id.clone(),
            recording_id: self.recording_id.clone(),
            entity_path: self.entity_path.clone(),
            spawn_viewer: self.spawn_viewer,
            grpc_address: self.grpc_address.clone(),
            recorder: self.recorder.clone(),
        }
    }
}

// Runtime state
struct State {
    rec: rerun::RecordingStream,
    entity_path: String,
    codec_sent: bool, // Track if H.264 codec has been sent
}

mod imp {
    use gst_video::prelude::VideoSinkExt;

    use super::*;

    // Main sink structure
    #[derive(Default)]
    pub struct RerunSink {
        pub(super) settings: Mutex<Settings>,
        pub(super) state: Mutex<Option<State>>,
    }

    // Register our type with GObject system
    #[glib::object_subclass]
    impl ObjectSubclass for RerunSink {
        const NAME: &'static str = "GstRerunSink";
        type Type = super::RerunSink;
        type ParentType = gst_video::VideoSink;
    }

    // GObject virtual methods implementation
    impl ObjectImpl for RerunSink {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
                vec![
                    glib::ParamSpecString::builder("app-id")
                        .nick("Application ID")
                        .blurb("Rerun application identifier")
                        .default_value(None)
                        .mutable_ready()
                        .build(),
                    glib::ParamSpecString::builder("recording-id")
                        .nick("Recording ID")
                        .blurb("Optional recording identifier for the recording stream")
                        .default_value(None)
                        .mutable_ready()
                        .build(),
                    glib::ParamSpecString::builder("entity-path")
                        .nick("Entity Path")
                        .blurb("Entity path for logging the frames")
                        .default_value(None)
                        .mutable_ready()
                        .build(),
                    glib::ParamSpecBoolean::builder("spawn-viewer")
                        .nick("Spawn Viewer")
                        .blurb("Spawn the rerun viewer if needed")
                        .default_value(DEFAULT_SPAWN_VIEWER)
                        .mutable_ready()
                        .build(),
                    glib::ParamSpecString::builder("grpc-address")
                        .nick("gRPC Address")
                        .blurb("gRPC server address")
                        .default_value(Some(DEFAULT_GRPC_ADDRESS))
                        .mutable_ready()
                        .build(),
                ]
            });

            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            let mut settings = self.settings.lock().unwrap();

            match pspec.name() {
                "app-id" => {
                    let app_id = value.get().expect("type checked upstream");
                    gst::info!(CAT, imp = self, "Set app-id: {:?}", app_id);
                    settings.app_id = app_id;
                }
                "recording-id" => {
                    let recording_id = value.get().expect("type checked upstream");
                    gst::info!(CAT, imp = self, "Set recording-id: {:?}", recording_id);
                    settings.recording_id = recording_id;
                }
                "entity-path" => {
                    let entity_path = value.get().expect("type checked upstream");
                    gst::info!(CAT, imp = self, "Set entity-path: {:?}", entity_path);
                    settings.entity_path = entity_path;
                }
                "spawn-viewer" => {
                    let spawn_viewer = value.get().expect("type checked upstream");
                    gst::info!(CAT, imp = self, "Set spawn-viewer: {}", spawn_viewer);
                    settings.spawn_viewer = spawn_viewer;
                }
                "grpc-address" => {
                    let grpc_address: String = value.get().expect("type checked upstream");
                    gst::info!(CAT, imp = self, "Set grpc-address: {}", grpc_address);
                    settings.grpc_address = grpc_address;
                }
                _ => unimplemented!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let settings = self.settings.lock().unwrap();

            match pspec.name() {
                "app-id" => settings.app_id.to_value(),
                "recording-id" => settings.recording_id.to_value(),
                "entity-path" => settings.entity_path.to_value(),
                "spawn-viewer" => settings.spawn_viewer.to_value(),
                "grpc-address" => settings.grpc_address.to_value(),
                _ => unimplemented!(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_show_preroll_frame(false);
        }
    }

    impl GstObjectImpl for RerunSink {}

    // Element implementation
    impl ElementImpl for RerunSink {
        fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
            static ELEMENT_METADATA: Lazy<gst::subclass::ElementMetadata> = Lazy::new(|| {
                gst::subclass::ElementMetadata::new(
                    "RerunSink",
                    "Sink/Video",
                    "Video sink that logs frames to rerun",
                    "Simon Guillot <simon@eigenscape.com>",
                )
            });

            Some(&*ELEMENT_METADATA)
        }

        fn pad_templates() -> &'static [gst::PadTemplate] {
            static PAD_TEMPLATES: Lazy<Vec<gst::PadTemplate>> = Lazy::new(|| {
                // Raw video formats
                let mut caps = gst_video::VideoCapsBuilder::new()
                    .format_list([
                        gst_video::VideoFormat::Nv12,
                        gst_video::VideoFormat::I420,
                        gst_video::VideoFormat::Rgb,
                        gst_video::VideoFormat::Gray8,
                        gst_video::VideoFormat::Rgba,
                    ])
                    .build();

                // Encoded video formats
                let h264_caps = gst::Caps::builder("video/x-h264")
                    .field("stream-format", "byte-stream")
                    .build();

                let h265_caps = gst::Caps::builder("video/x-h265")
                    .field(
                        "stream-format",
                        gst::List::new(["hvc1", "hev1", "byte-stream"]),
                    )
                    .build();

                caps.merge(h264_caps);
                caps.merge(h265_caps);

                vec![gst::PadTemplate::new(
                    "sink",
                    gst::PadDirection::Sink,
                    gst::PadPresence::Always,
                    &caps,
                )
                .unwrap()]
            });

            PAD_TEMPLATES.as_ref()
        }
    }

    impl BaseSinkImpl for RerunSink {
        fn start(&self) -> Result<(), gst::ErrorMessage> {
            let mut state = self.state.lock().unwrap();

            let (rec, entity_path) = {
                let settings = self.settings.lock().unwrap();

                let entity_path = settings.entity_path.clone().ok_or_else(|| {
                    gst::error_msg!(
                        gst::ResourceError::Settings,
                        ["entity-path property must be set before starting"]
                    )
                })?;

                let rec = if let Some(ref recorder) = settings.recorder {
                    gst::info!(CAT, imp = self, "Using shared recorder");
                    recorder.clone()
                } else {
                    // Extract values we need from settings
                    let app_id = settings.app_id.clone();
                    let recording_id = settings.recording_id.clone();
                    let spawn_viewer = settings.spawn_viewer;
                    let grpc_address = settings.grpc_address.clone();

                    // Drop the lock before creating the recorder
                    drop(settings);

                    let app_id_str = app_id.as_deref().unwrap_or("gst-rerun");

                    // Create builder and optionally set recording_id
                    let mut builder = rerun::RecordingStreamBuilder::new(app_id_str);
                    if let Some(ref rec_id) = recording_id {
                        builder = builder.recording_id(rec_id.as_str());
                    }

                    if spawn_viewer {
                        gst::info!(CAT, imp = self, "Spawning Rerun viewer");
                        builder.spawn().map_err(|e| {
                            gst::error_msg!(
                                gst::ResourceError::OpenWrite,
                                ["Error spawning Rerun viewer: {}", e]
                            )
                        })?
                    } else {
                        gst::info!(CAT, imp = self, "Connecting to gRPC at: {}", grpc_address);
                        builder.connect_grpc_opts(&grpc_address).map_err(|e| {
                            gst::error_msg!(
                                gst::ResourceError::OpenWrite,
                                ["Failed to connect to gRPC '{}': {}", grpc_address, e]
                            )
                        })?
                    }
                };

                (rec, entity_path)
            };

            *state = Some(State {
                rec,
                entity_path,
                codec_sent: false,
            });

            gst::info!(CAT, imp = self, "rerun sink started");
            Ok(())
        }

        fn stop(&self) -> Result<(), gst::ErrorMessage> {
            let mut state = self.state.lock().unwrap();

            if state.take().is_some() {
                gst::info!(CAT, imp = self, "rerun sink stopped");
            }

            Ok(())
        }

        fn render(&self, buffer: &gst::Buffer) -> Result<gst::FlowSuccess, gst::FlowError> {
            let state = self.state.lock().unwrap();
            if let Some(ref state) = *state {
                let pts = buffer.pts().ok_or_else(|| {
                    gst::error!(CAT, imp = self, "Buffer has no PTS");
                    gst::FlowError::Error
                })?;

                state
                    .rec
                    .set_time("pts", std::time::Duration::from_nanos(pts.nseconds()));
            }
            drop(state);

            // Get current caps
            let caps = self.obj().sink_pad().current_caps().ok_or_else(|| {
                gst::error!(CAT, imp = self, "Failed to get caps");
                gst::FlowError::Error
            })?;

            // Try to process as encoded frame first
            match self.process_encoded_frame(buffer, &caps) {
                Ok(()) => return Ok(gst::FlowSuccess::Ok),
                Err(RerunSinkError::UnsupportedEncodingFormat(_)) => {
                    // Fall through to try raw video
                }
                Err(e) => {
                    gst::error!(CAT, imp = self, "Failed to process encoded frame: {}", e);
                    return Err(match e {
                        RerunSinkError::UnsupportedVideoFormat(_) => gst::FlowError::NotNegotiated,
                        _ => gst::FlowError::Error,
                    });
                }
            }

            // Try to process as raw video
            self.process_frame(buffer, &caps).map_err(|e| {
                gst::error!(CAT, imp = self, "Failed to process frame: {}", e);
                match e {
                    RerunSinkError::VideoInfoFromCapsFailed => gst::FlowError::NotNegotiated,
                    RerunSinkError::UnsupportedVideoFormat(_) => gst::FlowError::NotNegotiated,
                    _ => gst::FlowError::Error,
                }
            })?;

            Ok(gst::FlowSuccess::Ok)
        }
    }

    impl VideoSinkImpl for RerunSink {}

    // Helper methods
    impl RerunSink {
        fn process_encoded_frame(
            &self,
            buffer: &gst::Buffer,
            caps: &gst::Caps,
        ) -> Result<(), RerunSinkError> {
            let mut state = self.state.lock().unwrap();

            let Some(ref mut state) = *state else {
                return Ok(());
            };

            let structure = caps.structure(0).unwrap();
            let format_name = structure.name().as_str();

            let codec = match format_name {
                "video/x-h264" => rerun::components::VideoCodec::H264,
                "video/x-h265" => rerun::components::VideoCodec::H265,
                _ => {
                    return Err(RerunSinkError::UnsupportedEncodingFormat(
                        format_name.to_string(),
                    ))
                }
            };

            if !state.codec_sent {
                let stream_format: String = structure
                    .get("stream-format")
                    .unwrap_or_else(|_| "unknown".to_string());
                gst::info!(
                    CAT,
                    imp = self,
                    "format {} detected, stream-format: {}, using {:?}",
                    format_name,
                    stream_format,
                    codec
                );
                state
                    .rec
                    .log_static(state.entity_path.as_str(), &rerun::VideoStream::new(codec))?;
                state.codec_sent = true;
            }

            // Map buffer to get data
            let map = buffer
                .map_readable()
                .map_err(|_| RerunSinkError::BufferMapFailed)?;

            // Log the video frame
            state.rec.log(
                state.entity_path.as_str(),
                &rerun::VideoStream::new(codec).with_sample(map.as_slice()),
            )?;

            Ok(())
        }

        fn process_frame(
            &self,
            buffer: &gst::Buffer,
            caps: &gst::Caps,
        ) -> Result<(), RerunSinkError> {
            let state = self.state.lock().unwrap();

            let Some(ref state) = *state else {
                return Ok(());
            };

            let info = gst_video::VideoInfo::from_caps(caps)
                .map_err(|_| RerunSinkError::VideoInfoFromCapsFailed)?;

            let map = buffer
                .map_readable()
                .map_err(|_| RerunSinkError::BufferMapFailed)?;

            let data = map.as_slice();
            let width = info.width();
            let height = info.height();
            let format = info.format();

            let image = Self::new_image_from_frame(data, format, width, height)?;
            state.rec.log(state.entity_path.as_str(), &image)?;

            Ok(())
        }

        fn new_image_from_frame(
            data: &[u8],
            format: gst_video::VideoFormat,
            width: u32,
            height: u32,
        ) -> Result<rerun::Image, RerunSinkError> {
            match format {
                gst_video::VideoFormat::Rgb => Ok(rerun::Image::from_rgb24(data, [width, height])),
                gst_video::VideoFormat::Rgba => {
                    Ok(rerun::Image::from_rgba32(data, [width, height]))
                }
                gst_video::VideoFormat::Gray8 => Ok(rerun::Image::from_l8(data, [width, height])),
                gst_video::VideoFormat::Nv12 | gst_video::VideoFormat::I420 => {
                    // For NV12 and I420, log as raw NV12
                    use rerun::components::ImageFormat;
                    use rerun::datatypes::PixelFormat;

                    let format = ImageFormat(rerun::datatypes::ImageFormat {
                        width,
                        height,
                        pixel_format: Some(PixelFormat::NV12),
                        color_model: None,
                        channel_datatype: None,
                    });

                    Ok(rerun::Image::new(data, format))
                }
                fmt => Err(RerunSinkError::UnsupportedVideoFormat(fmt)),
            }
        }
    }
}

// The public Rust wrapper type for our sink element
glib::wrapper! {
    pub struct RerunSink(ObjectSubclass<imp::RerunSink>)
        @extends gst_video::VideoSink, gst_base::BaseSink, gst::Element, gst::Object;
}

impl RerunSink {
    pub fn new(name: Option<&str>) -> Self {
        glib::Object::builder().property("name", name).build()
    }

    /// Set a shared RecordingStream to be used by this sink
    pub fn set_recorder(&self, recorder: rerun::RecordingStream) {
        let imp = self.imp();
        let mut settings = imp.settings.lock().unwrap();
        gst::info!(CAT, imp = imp, "Setting shared recorder");
        settings.recorder = Some(recorder);
    }
}

// Register the element with GStreamer
pub fn register(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "rerunsink",
        gst::Rank::NONE,
        RerunSink::static_type(),
    )
}
