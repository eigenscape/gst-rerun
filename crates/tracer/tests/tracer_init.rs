use gst::prelude::*;
use gst_reruntracer::plugin_register_static;
use tracing_test::traced_test;

#[traced_test]
#[test]
fn test_tracer_registered_and_created() {
    tracing_gstreamer::integrate_events();
    gst::log::remove_default_log_function();
    gst::log::set_default_threshold(gst::DebugLevel::Info);

    plugin_register_static().expect("Failed to register tracer plugin");
    std::env::set_var(
        "GST_TRACERS",
        "reruntracing(debounce=100,spawn-viewer=false)",
    );
    gst::init().expect("Failed to initialize GStreamer");
    tracing_gstreamer::integrate_spans();

    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("fakesrc").build().unwrap();
    let sink = gst::ElementFactory::make("fakesink").build().unwrap();

    pipeline.add_many([&src, &sink]).unwrap();
    src.link(&sink).unwrap();

    assert!(logs_contain("RerunTracer ready"));
}
